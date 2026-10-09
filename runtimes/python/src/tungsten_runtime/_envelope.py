# SPDX-License-Identifier: Apache-2.0
"""The diagnostic error envelope (planning/06): construction with the
documented member order, default ``retryable`` per category, the status to
category mapping and the generic remediation texts used when neither the
operation nor the API has a specific entry (``runtimes/ts/src/envelope.ts``).

Envelope identifiers are shared by every runtime except where they name
something the caller passes: ``failed_parameter`` names the call options as
this SDK spells them (``idempotency_key``, where the TypeScript runtime says
``idempotencyKey``; ``confirm``), the descriptors (``operation``,
``macro``), the whole ``args`` or macro ``input``, and paths
``args.<name>`` (the Python argument name) / ``body.<path>`` (wire names) /
``response.<path>``. Remediation prose names the Python spellings of SDK
options (``idempotency_key``, ``confirm=True``, ``ClientOptions.base_url``).
"""

from __future__ import annotations

from collections.abc import Collection
from typing import Final, TypeGuard

from ._json import display_json, parse_json
from ._util import REDACTED
from .types import Category, Diagnostic, Retryable

CATEGORIES: Final[tuple[Category, ...]] = (
    "VALIDATION_FAILED",
    "MALFORMED_REQUEST",
    "REQUEST_TOO_LARGE",
    "AUTH_FAILED",
    "NOT_FOUND",
    "CONFLICT",
    "PRECONDITION_FAILED",
    "RATE_LIMITED",
    "UPSTREAM_UNAVAILABLE",
    "OUTCOME_UNKNOWN",
    "TRANSPORT_FAILED",
    "CONFIRMATION_REQUIRED",
    "GATE_DISABLED",
    "UNEXPECTED_RESPONSE",
)

RETRYABLE_VALUES: Final[tuple[Retryable, ...]] = (
    "never",
    "after_delay",
    "same_key_only",
    "after_remediation",
)

#: Default ``retryable`` per category (planning/06 table).
DEFAULT_RETRYABLE: Final[dict[Category, Retryable]] = {
    "VALIDATION_FAILED": "never",
    "MALFORMED_REQUEST": "never",
    "REQUEST_TOO_LARGE": "never",
    "AUTH_FAILED": "never",
    "NOT_FOUND": "never",
    "CONFLICT": "never",
    "PRECONDITION_FAILED": "never",
    "RATE_LIMITED": "after_delay",
    "UPSTREAM_UNAVAILABLE": "after_delay",
    "OUTCOME_UNKNOWN": "same_key_only",
    "TRANSPORT_FAILED": "after_delay",
    "CONFIRMATION_REQUIRED": "never",
    "GATE_DISABLED": "never",
    "UNEXPECTED_RESPONSE": "never",
}

#: Remediation used when no manifest entry applies.
GENERIC_REMEDIATION: Final[dict[Category, str]] = {
    "VALIDATION_FAILED": (
        "The server rejected the arguments. Fix the parameter the API error names and call again; "
        "the same request fails the same way."
    ),
    "MALFORMED_REQUEST": (
        "The server could not parse the request. Check the format of path and query parameters "
        "(for example canonical UUIDs); do not retry unchanged."
    ),
    "REQUEST_TOO_LARGE": "The request body is larger than the server accepts. Send a smaller body; do not retry unchanged.",
    "AUTH_FAILED": (
        "The credential is missing, expired, revoked or lacks permission. Do not retry; fix the credential "
        "configured in ClientOptions.auth."
    ),
    "NOT_FOUND": (
        "The resource does not exist or is not visible to this credential. Check the identifiers; "
        "do not retry unchanged."
    ),
    "CONFLICT": (
        "The request conflicts with the resource's current state. Read the current state before deciding "
        "what to do; do not retry unchanged."
    ),
    "PRECONDITION_FAILED": (
        "A precondition of this operation is not met (account, billing or resource state). Resolve it first; "
        "do not retry unchanged."
    ),
    "RATE_LIMITED": "Rate limited. Wait retry_after_ms (or a few seconds when it is null) before calling again.",
    "UPSTREAM_UNAVAILABLE": "The service is temporarily unavailable. Wait, then call again.",
    "OUTCOME_UNKNOWN": (
        "The server may or may not have applied this call. Check whether it took effect before doing anything else."
    ),
    "TRANSPORT_FAILED": (
        "The request could not be delivered (DNS, TLS or connection failure), so the server did not receive it. "
        "Check base_url and network access, then call again."
    ),
    "CONFIRMATION_REQUIRED": "This operation needs confirmation: call preview(...) and pass its confirmation_token.",
    "GATE_DISABLED": "This operation is disabled on this deployment. It is a deployment setting; do not retry.",
    "UNEXPECTED_RESPONSE": (
        "The server's response did not match the API description. Do not retry blindly; report it."
    ),
}


def is_category(value: object) -> TypeGuard[Category]:
    return isinstance(value, str) and value in CATEGORIES


def is_retryable(value: object) -> TypeGuard[Retryable]:
    return isinstance(value, str) and value in RETRYABLE_VALUES


def diagnostic(
    operation: str,
    category: Category,
    *,
    http_status: int | None = None,
    code: str | None = None,
    failed_parameter: str | None = None,
    received_value: object = None,
    expected: str | None = None,
    remediation: str | None = None,
    retryable: Retryable | None = None,
    retry_after_ms: int | None = None,
    next_action: str | None = None,
    request_id: str | None = None,
    attempts: int = 0,
) -> Diagnostic:
    """An envelope with every member present, in the documented order."""
    return {
        "status": "error",
        "category": category,
        "operation": operation,
        "http_status": http_status,
        "code": code,
        "failed_parameter": failed_parameter,
        "received_value": received_value,
        "expected": expected,
        "remediation": remediation if remediation is not None else GENERIC_REMEDIATION[category],
        "retryable": retryable if retryable is not None else DEFAULT_RETRYABLE[category],
        "retry_after_ms": retry_after_ms,
        "next_action": next_action,
        "request_id": request_id,
        "trace": {"attempts": attempts},
    }


def category_for_status(status: int, json_body: bool) -> Category:
    """Category of an HTTP error status when no manifest entry says otherwise."""
    match status:
        case 400:
            return "VALIDATION_FAILED" if json_body else "MALFORMED_REQUEST"
        case 401 | 403 | 407:
            return "AUTH_FAILED"
        case 404 | 410:
            return "NOT_FOUND"
        case 409:
            return "CONFLICT"
        case 402 | 412 | 428:
            return "PRECONDITION_FAILED"
        case 413:
            return "REQUEST_TOO_LARGE"
        case 422:
            return "VALIDATION_FAILED"
        case 429:
            return "RATE_LIMITED"
        case 408 | 502 | 503 | 504:
            return "UPSTREAM_UNAVAILABLE"
        case _:
            pass
    if 500 <= status <= 599:
        return "UPSTREAM_UNAVAILABLE"
    if 400 <= status <= 499:
        return "VALIDATION_FAILED" if json_body else "MALFORMED_REQUEST"
    return "UNEXPECTED_RESPONSE"


def scrub_text(text: str, secrets: Collection[str]) -> str:
    """Replace every occurrence of the given secrets (four characters or more)."""
    out = text
    for secret in secrets:
        if len(secret) >= 4 and secret in out:
            out = out.replace(secret, REDACTED)
    return out


def scrub_diagnostic(d: Diagnostic, secrets: Collection[str]) -> Diagnostic:
    """Replace every occurrence of the given secrets in the envelope's string
    members. A last line of defence: no member is built from a secret."""
    if len(secrets) == 0:
        return d

    def scrub(text: str | None) -> str | None:
        return None if text is None else scrub_text(text, secrets)

    def scrub_value(value: object) -> object:
        if isinstance(value, str):
            return scrub_text(value, secrets)
        if value is None or isinstance(value, bool | int | float):
            return value
        try:
            return parse_json(scrub_text(display_json(value), secrets))
        except ValueError:
            return REDACTED

    out: Diagnostic = {
        "status": d["status"],
        "category": d["category"],
        "operation": d["operation"],
        "http_status": d["http_status"],
        "code": scrub(d["code"]),
        "failed_parameter": scrub(d["failed_parameter"]),
        "received_value": scrub_value(d["received_value"]),
        "expected": scrub(d["expected"]),
        "remediation": scrub_text(d["remediation"], secrets),
        "retryable": d["retryable"],
        "retry_after_ms": d["retry_after_ms"],
        "next_action": scrub(d["next_action"]),
        "request_id": scrub(d["request_id"]),
        "trace": {"attempts": d["trace"]["attempts"]},
    }
    return out


def with_fields(d: Diagnostic, **changes: object) -> Diagnostic:
    """A copy of the envelope with some members replaced, keeping the order."""
    out = dict(d)
    for key, value in changes.items():
        if key not in out:
            raise KeyError(key)
        out[key] = value
    return out  # pyright: ignore[reportReturnType]
