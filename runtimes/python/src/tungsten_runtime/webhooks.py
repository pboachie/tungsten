# SPDX-License-Identifier: Apache-2.0
"""Incoming webhook verification with no framework dependency.

The first adapter is the Stripe-compatible signature scheme: a header such as
``Stripe-Signature: t=1700000000,v1=<hex>`` where ``v1`` is the lowercase hex
HMAC-SHA256 of ``"<t>.<raw body>"`` under the endpoint secret. Pass the raw
request bytes exactly as received, before any JSON parsing.

Checks run in a fixed order, identical in every tungsten runtime, and the
first failure is reported: body size, secret presence, header presence,
header syntax, signature, timestamp tolerance, JSON payload, typed decoding
(``parse``), replay claim.
Failures carry a stable ``reason`` and a message that never contains the
secret, the signature or the payload.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import re
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Any, Literal

__all__ = [
    "DEFAULT_WEBHOOK_MAX_BODY_BYTES",
    "DEFAULT_WEBHOOK_TOLERANCE_SECONDS",
    "MAX_WEBHOOK_SIGNATURE_HEADER",
    "RejectedWebhook",
    "VerifiedWebhook",
    "WebhookOutcome",
    "WebhookRejection",
    "sign_webhook",
    "verify_webhook",
]

DEFAULT_WEBHOOK_MAX_BODY_BYTES = 1_048_576
"""Default largest accepted body: 1 MiB."""
DEFAULT_WEBHOOK_TOLERANCE_SECONDS = 300
"""Default allowed distance between the signed timestamp and now: 5 minutes."""
MAX_WEBHOOK_SIGNATURE_HEADER = 8192
"""Longest accepted signature header, in UTF-16 code units (ASCII bytes)."""

WebhookRejection = Literal[
    "body_too_large",
    "secret_missing",
    "signature_missing",
    "signature_malformed",
    "signature_mismatch",
    "timestamp_out_of_tolerance",
    "duplicate",
    "payload_invalid",
]

_TIMESTAMP = re.compile(r"[0-9]{1,15}")
_HEX = re.compile(r"[0-9a-fA-F]{64}")
# The whitespace JavaScript's String.prototype.trim removes, so header items
# split the same way in every runtime.
_JS_WHITESPACE = "\t\n\v\f\r                  　﻿"


@dataclass(frozen=True)
class VerifiedWebhook:
    """A verified, parsed delivery."""

    event: Any
    """The parsed JSON payload."""
    timestamp: int
    """The signed timestamp (``t``), Unix seconds."""
    type: str | None
    """The payload's string ``type``, or None."""
    id: str | None
    """The payload's string ``id``, or None."""
    ok: Literal[True] = True


@dataclass(frozen=True)
class RejectedWebhook:
    """A rejected delivery; safe to log."""

    reason: WebhookRejection
    message: str
    ok: Literal[False] = False


WebhookOutcome = VerifiedWebhook | RejectedWebhook


def _positive(value: int | None, fallback: int, name: str) -> int:
    if value is None:
        return fallback
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0 or value > 2**53 - 1:
        raise ValueError(f"{name} must be a positive integer")
    return value


def _parse_header(header: str) -> tuple[str, list[bytes]] | str:
    if len(header.encode("utf-16-le", "surrogatepass")) // 2 > MAX_WEBHOOK_SIGNATURE_HEADER:
        return "the signature header is too long"
    timestamp: str | None = None
    signatures: list[bytes] = []
    for raw in header.split(","):
        item = raw.strip(_JS_WHITESPACE)
        eq = item.find("=")
        if eq <= 0:
            return "the signature header has an item without a key"
        key, value = item[:eq], item[eq + 1 :]
        if key == "t":
            if timestamp is not None:
                return "the signature header has more than one timestamp"
            if not _TIMESTAMP.fullmatch(value):
                return "the signature header timestamp is not a decimal integer"
            timestamp = value
        elif key == "v1" and _HEX.fullmatch(value):
            signatures.append(bytes.fromhex(value))
    if timestamp is None:
        return "the signature header has no timestamp"
    if not signatures:
        return "the signature header has no v1 signature"
    return timestamp, signatures


def _no_constant(name: str) -> Any:
    raise ValueError(name)


def verify_webhook(
    body: bytes | str,
    signature_header: str | None,
    secrets: str | Sequence[str],
    *,
    tolerance_seconds: int | None = None,
    max_body_bytes: int | None = None,
    now: int | None = None,
    claim: Callable[[str], bool] | None = None,
    parse: Callable[[Any], Any] | None = None,
) -> WebhookOutcome:
    """Verify a Stripe-compatible signed delivery and parse its JSON payload.

    ``secrets`` is the endpoint secret, or several during rotation (any one
    may match). ``claim`` is application-owned replay tracking, called only
    after the signature and timestamp are verified: return False when the key
    was already accepted and the delivery is rejected as ``duplicate``. The
    key is the event's string ``id`` when present, otherwise
    ``"<t>.<matched v1 signature>"``. ``parse`` is typed decoding of the parsed
    JSON payload, run before ``claim``: return the typed event, or raise to
    reject the delivery as ``payload_invalid``.

    Never raises for a bad delivery; raises ``ValueError`` for invalid options.
    """
    tolerance = _positive(tolerance_seconds, DEFAULT_WEBHOOK_TOLERANCE_SECONDS, "tolerance_seconds")
    max_body = _positive(max_body_bytes, DEFAULT_WEBHOOK_MAX_BODY_BYTES, "max_body_bytes")
    raw = body.encode("utf-8", "surrogatepass") if isinstance(body, str) else bytes(body)
    if len(raw) > max_body:
        return RejectedWebhook("body_too_large", f"the body exceeds {max_body} bytes")
    keys = [secrets] if isinstance(secrets, str) else list(secrets)
    keys = [k for k in keys if k]
    if not keys:
        return RejectedWebhook("secret_missing", "no webhook secret is configured")
    if signature_header is None or signature_header.strip(_JS_WHITESPACE) == "":
        return RejectedWebhook("signature_missing", "the request has no signature header")
    parsed = _parse_header(signature_header)
    if isinstance(parsed, str):
        return RejectedWebhook("signature_malformed", parsed)
    t, signatures = parsed
    signed = t.encode("ascii") + b"." + raw
    matched: bytes | None = None
    for secret in keys:
        digest = hmac.new(secret.encode("utf-8", "surrogatepass"), signed, hashlib.sha256).digest()
        for signature in signatures:
            if hmac.compare_digest(digest, signature) and matched is None:
                matched = signature
    if matched is None:
        return RejectedWebhook("signature_mismatch", "no signature matches a configured secret")
    timestamp = int(t)
    current = int(time.time()) if now is None else now
    if abs(current - timestamp) > tolerance:
        return RejectedWebhook(
            "timestamp_out_of_tolerance",
            f"the signed timestamp is more than {tolerance} seconds from now",
        )
    try:
        text = raw.decode("utf-8")
        if text.startswith("﻿"):
            raise ValueError("BOM")
        event = json.loads(text, parse_constant=_no_constant)
    except ValueError:
        return RejectedWebhook("payload_invalid", "the body is not UTF-8 JSON")
    record = event if isinstance(event, dict) else None
    event_id = record.get("id") if record is not None and isinstance(record.get("id"), str) else None
    event_type = record.get("type") if record is not None and isinstance(record.get("type"), str) else None
    if parse is not None:
        try:
            event = parse(event)
        except Exception:
            return RejectedWebhook("payload_invalid", "the payload does not match the event type")
    if claim is not None and not claim(event_id if event_id is not None else f"{t}.{matched.hex()}"):
        return RejectedWebhook("duplicate", "the delivery was already accepted")
    return VerifiedWebhook(event=event, timestamp=timestamp, type=event_type, id=event_id)


def sign_webhook(body: bytes | str, secret: str, timestamp: int) -> str:
    """Build a Stripe-compatible signature header for ``body`` at ``timestamp``.

    Intended for local examples and outbound test deliveries.
    """
    if isinstance(timestamp, bool) or not isinstance(timestamp, int) or timestamp < 0 or timestamp > 2**53 - 1:
        raise ValueError("timestamp must be a non-negative integer")
    raw = body.encode("utf-8", "surrogatepass") if isinstance(body, str) else bytes(body)
    t = str(timestamp)
    digest = hmac.new(secret.encode("utf-8", "surrogatepass"), t.encode("ascii") + b"." + raw, hashlib.sha256)
    return f"t={t},v1={digest.hexdigest()}"
