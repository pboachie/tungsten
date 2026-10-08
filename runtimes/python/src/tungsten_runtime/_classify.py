# SPDX-License-Identifier: Apache-2.0
"""Classification of HTTP responses and lost requests into results and
diagnostic envelopes (planning/06 "Diagnostic error envelope", "Unknown
outcome", planning/04 "Remediation table resolution";
``runtimes/ts/src/classify.ts``)."""

from __future__ import annotations

import math
import re
from collections.abc import Mapping
from dataclasses import dataclass

from ._envelope import GENERIC_REMEDIATION, category_for_status, diagnostic, is_category, is_retryable
from ._json import display_json, js_number, parse_json
from ._util import field, get_path, is_array, is_number, is_record, items_of, str_field
from .sentinels import UNSET
from .types import Category, Diagnostic, OperationDescriptor, Retryable

#: Headers that carry a request id, in lookup order.
REQUEST_ID_HEADERS = ("x-request-id", "request-id", "x-correlation-id", "x-amzn-requestid", "cf-ray")


def request_id_of(headers: Mapping[str, str]) -> str | None:
    for name in REQUEST_ID_HEADERS:
        value = headers.get(name)
        if value:
            return value[:200]
    return None


def media_type_of(headers: Mapping[str, str]) -> str:
    """The media type without parameters, lowercased; "" when absent."""
    return headers.get("content-type", "").split(";")[0].strip().lower()


def is_json_media(media: str) -> bool:
    return media == "application/json" or media.endswith("+json") or media == "text/json"


def match_response(responses: object, status: int) -> Mapping[str, object] | None:
    """The response descriptor matching a status: exact, then ``NXX``, then ``default``."""
    listed = [r for r in responses if is_record(r)] if is_array(responses) else []
    for r in listed:
        if is_number(r.get("status")) and r.get("status") == status:
            return r
    family = f"{status // 100}XX"
    for r in listed:
        if r.get("status") == family:
            return r
    for r in listed:
        if r.get("status") == "default":
            return r
    return None


@dataclass(frozen=True, slots=True)
class DecodedBody:
    """A decoded response body."""

    #: Parsed JSON, text, bytes, or UNSET for an empty body.
    value: object
    json: bool
    #: JSON was announced but did not parse.
    invalid_json: bool
    empty: bool


_JSON_START = re.compile(r"\s*[\[{]")


def decode_body(data: bytes, headers: Mapping[str, str], declared: str | None) -> DecodedBody:
    if len(data) == 0:
        return DecodedBody(UNSET, False, False, True)
    media = media_type_of(headers) or (declared or "").lower()
    if is_json_media(media):
        raw = data.decode("utf-8", "replace")
        try:
            return DecodedBody(parse_json(raw), True, False, False)
        except ValueError:
            return DecodedBody(raw, False, True, False)
    if (
        media == ""
        or media.startswith("text/")
        or media == "application/problem+xml"
        or media.endswith("+xml")
        or media == "application/xml"
    ):
        raw = data.decode("utf-8", "replace")
        if media == "" and _JSON_START.match(raw) is not None:
            try:
                return DecodedBody(parse_json(raw), True, False, False)
            except ValueError:
                pass
        return DecodedBody(raw, False, False, False)
    return DecodedBody(data, False, False, False)


_MESSAGE_FIELDS = ("message", "error.message", "detail", "error_description", "title", "error")


def _server_message(body: object) -> str | None:
    for path in _MESSAGE_FIELDS:
        value = get_path(body, path)
        if isinstance(value, str) and value.strip() != "":
            return value.strip()[:200]
    return None


def _error_code(op: OperationDescriptor, body: object) -> str | None:
    path = field(op, "error_code_field")
    if not isinstance(path, str) or path == "":
        return None
    value = get_path(body, path)
    if isinstance(value, str) and value != "":
        return value[:200]
    if is_number(value) and math.isfinite(value):
        return js_number(float(value)) if isinstance(value, float) else str(int(value))
    return None


def _remediation_entry(table: object, code: str | None) -> Mapping[str, object] | None:
    if code is None or not is_record(table):
        return None
    entry = table.get(code)
    return entry if is_record(entry) else None


@dataclass(frozen=True, slots=True)
class OutcomeCheck:
    """How to find out whether an unprotected mutation took effect, in words:
    ``call`` names the read and its arguments, ``shows`` what to look for."""

    call: str
    shows: str


@dataclass(frozen=True, slots=True)
class CallContext:
    """Context shared by every classification of one call."""

    api: object
    op: OperationDescriptor
    #: Key sent with the request, if any (never shown).
    key: str | None
    #: The key's header name, for remediation text.
    key_header: str
    attempts: int
    #: For a mutation without replay protection: the read that shows whether it took effect.
    check: OutcomeCheck | None = None


def _refers_to_response(node: object, depth: int) -> bool:
    if depth > 32:
        return True
    if isinstance(node, str):
        return node == "$response" or node.startswith("$response.")
    if is_array(node):
        return any(_refers_to_response(item, depth + 1) for item in node)
    if is_record(node):
        return any(_refers_to_response(item, depth + 1) for item in node.values())
    return False


def verify_callable(verify: object) -> bool:
    """Whether the verification hook's arguments can be built without the
    call's response: after a lost or ambiguous answer there is no success
    body, so a hook whose args reference ``$response`` cannot be called."""
    return is_record(verify) and not _refers_to_response(verify.get("args"), 0)


def policy_of(op: object) -> object:
    return field(field(field(op, "agent"), "idempotency"), "policy")


def safety_of(op: object) -> object:
    return field(field(op, "agent"), "safety")


def has_replay_protection(op: object, key: str | None) -> bool:
    """Whether the operation sends an idempotency key or an identity body,
    so resending it cannot apply its effect twice."""
    return policy_of(op) == "content_identity" or key is not None


def is_mutation(op: object) -> bool:
    return safety_of(op) != "read_only"


def _next_action_hint(ctx: CallContext) -> str | None:
    """What resolves an unknown outcome: the verification hook when it can be
    called without the lost response, else repeating the call under its
    replay protection. None when nothing can resolve it safely."""
    op_id = ctx.op["id"]
    verify = field(field(ctx.op, "agent"), "verify")
    target = str_field(verify, "operation")
    if target is not None and verify_callable(verify):
        args = field(verify, "args")
        keys = list(args.keys()) if is_record(args) else []
        with_args = f" with {', '.join(keys)}" if len(keys) > 0 else ""
        return f"Call {target}{with_args} to check whether {op_id} took effect before doing anything else."
    if policy_of(ctx.op) == "content_identity":
        return (
            f"Call {op_id} again with the identical body; the body is its own identity, so the server answers "
            "the original result instead of applying it twice."
        )
    if ctx.key is not None:
        return (
            f"Call {op_id} again with the same {ctx.key_header} value and identical arguments; the server answers "
            "the original result instead of applying it twice."
        )
    return None


def change_of(op: OperationDescriptor) -> str:
    """The change an operation makes, in words: its summary when it has one."""
    summary_value = field(op, "summary")
    summary = summary_value.strip() if isinstance(summary_value, str) else ""
    if summary.endswith("."):
        summary = summary[:-1]
    return f"the change {op['id']} makes" if summary == "" else f"the change {display_json(summary)}"


def outcome_unknown(
    ctx: CallContext,
    cause: str,
    *,
    http_status: int | None = None,
    code: str | None = None,
    request_id: str | None = None,
    retry_after_ms: int | None = None,
    next_action: str | None = None,
) -> Diagnostic:
    """OUTCOME_UNKNOWN for a mutation whose effect cannot be known. Without
    replay protection a repeat can apply the effect twice, so the envelope
    never offers one: ``retryable`` is ``after_remediation`` and the
    remediation and ``next_action`` say what to check before repeating."""
    op = ctx.op
    op_id = op["id"]
    hint: str | None = None
    if policy_of(op) == "content_identity":
        rule = "If you retry, resend the identical bytes only; the body is its own identity."
    elif ctx.key is not None:
        rule = f"If you retry, reuse the SAME {ctx.key_header} value; a new key can apply the effect twice."
    elif ctx.check is not None:
        call, shows = ctx.check.call, ctx.check.shows
        rule = (
            f"This operation has no idempotency key, so repeating it can apply the effect twice. Before calling "
            f"{op_id} again, call {call} and check {shows}: if it does, {op_id} took effect and must not be "
            f"repeated; call it again only if it did not."
        )
        hint = f"Call {call} and check {shows}; call {op_id} again only if it did not take effect."
    else:
        change = change_of(op)
        rule = (
            "This operation has no idempotency key, so repeating it can apply the effect twice: do not call it "
            "again until you have checked whether it took effect, by reading the resource it changes and looking "
            f"for {change}."
        )
        hint = f"Read the resource {op_id} changes and look for {change}; call {op_id} again only if it is not there."
    action = next_action if next_action is not None else hint if hint is not None else _next_action_hint(ctx)
    return diagnostic(
        op_id,
        "OUTCOME_UNKNOWN",
        remediation=f"{cause} The server may or may not have applied {op_id}. {rule}",
        retryable="same_key_only" if has_replay_protection(op, ctx.key) else "after_remediation",
        next_action=action,
        http_status=http_status,
        code=code,
        request_id=request_id,
        retry_after_ms=retry_after_ms,
        attempts=ctx.attempts,
    )


#: Categories that say the request was not applied, or that a repeat is
#: pointless: a manifest entry with one of them (or with ``retryable``
#: ``never`` / ``after_remediation``) is a definite answer even for a status
#: that is otherwise ambiguous.
_DEFINITE_CATEGORIES = frozenset(
    {
        "VALIDATION_FAILED",
        "MALFORMED_REQUEST",
        "REQUEST_TOO_LARGE",
        "AUTH_FAILED",
        "NOT_FOUND",
        "CONFLICT",
        "PRECONDITION_FAILED",
        "RATE_LIMITED",
        "GATE_DISABLED",
    }
)


def _definite(entry: Mapping[str, object] | None) -> bool:
    if entry is None:
        return False
    if entry.get("retryable") in ("never", "after_remediation"):
        return True
    category = entry.get("category")
    return isinstance(category, str) and category in _DEFINITE_CATEGORIES


def classify_error(
    ctx: CallContext,
    status: int,
    headers: Mapping[str, str],
    decoded: DecodedBody,
    retry_after_ms: int | None,
) -> Diagnostic:
    """The classification of an HTTP error status."""
    api, op = ctx.api, ctx.op
    op_id = op["id"]
    request_id = request_id_of(headers)
    attempts = ctx.attempts
    code = _error_code(op, decoded.value) if decoded.json else None

    # A disabled gate answers its status without the API's error document
    # (the route is not mounted). An answer with an error code comes from the
    # mounted route, so it is classified by its code like any other error.
    gate = op["status"]
    if gate["kind"] == "gated" and status == field(gate, "disabled_status") and code is None:
        env_var = str(field(gate, "env_var"))
        text = str_field(field(api, "gates"), env_var)
        return diagnostic(
            op_id,
            "GATE_DISABLED",
            http_status=status,
            request_id=request_id,
            retry_after_ms=retry_after_ms,
            attempts=attempts,
            code=code,
            remediation=text
            if text is not None
            else (
                f"This deployment disables {op_id} because {env_var} is off (HTTP {status}). It is a deployment "
                "setting, not a missing resource; do not retry."
            ),
            retryable="never",
        )

    entry = _remediation_entry(field(field(op, "agent"), "remediation"), code) or _remediation_entry(
        field(api, "error_codes"), code
    )
    media = "none" if decoded.empty else media_type_of(headers) or "none"
    non_json: Mapping[str, object] | None = None
    if not decoded.json:
        listed = field(api, "non_json")
        for n in items_of(listed):
            if (
                is_record(n)
                and is_number(n.get("status"))
                and n.get("status") == status
                and (n.get("media") == media or (n.get("media") == "none" and decoded.empty))
            ):
                non_json = n
                break
    declared = match_response(field(op, "responses"), status)
    ambiguous_statuses = field(api, "ambiguous_statuses")
    # On a mutation, a status listed as ambiguous, a response declared
    # ambiguous, and any 5xx leave the outcome unknown, unless the manifest's
    # entry for the answer's code or non-JSON shape says it is a definite
    # rejection.
    ambiguous = (
        (is_array(ambiguous_statuses) and any(is_number(s) and s == status for s in ambiguous_statuses))
        or (declared is not None and declared.get("kind") == "ambiguous")
        or 500 <= status <= 599
    )
    if ambiguous and is_mutation(op) and not _definite(entry if entry is not None else non_json):
        said = str_field(entry, "text") if entry is not None else str_field(non_json, "text")
        next_action = str_field(entry, "next_action")
        return outcome_unknown(
            ctx,
            f"HTTP {status} leaves the outcome of this call unknown.{f' {said}' if said else ''}",
            http_status=status,
            code=code,
            request_id=request_id,
            retry_after_ms=retry_after_ms,
            next_action=next_action,
        )

    retryable: Retryable | None = None
    text: str | None = None
    next_action: str | None = None
    category: Category
    if entry is not None:
        listed_category = entry.get("category")
        category = (
            listed_category if is_category(listed_category) else category_for_status(status, decoded.json)
        )
        listed_retryable = entry.get("retryable")
        retryable = listed_retryable if is_retryable(listed_retryable) else None
        text = str_field(entry, "text")
        next_action = str_field(entry, "next_action")
    elif non_json is not None and is_category(non_json.get("category")):
        category = non_json["category"]  # pyright: ignore[reportAssignmentType]
        listed_retryable = non_json.get("retryable")
        retryable = listed_retryable if is_retryable(listed_retryable) else None
        text = str_field(non_json, "text")
    else:
        category = category_for_status(status, decoded.json)
    if category == "OUTCOME_UNKNOWN" and not is_mutation(op):
        # A read has no effect whose outcome could be unknown: the answer only
        # says the service did not respond in time.
        category = "UPSTREAM_UNAVAILABLE"
        if retryable is None or retryable == "same_key_only":
            retryable = "after_delay"
    if text is None:
        said = _server_message(decoded.value) if decoded.json else None
        text = f"{GENERIC_REMEDIATION[category]}{f' Server message: {display_json(said)}' if said else ''}"
    return diagnostic(
        op_id,
        category,
        http_status=status,
        request_id=request_id,
        retry_after_ms=retry_after_ms,
        attempts=attempts,
        code=code,
        remediation=text,
        retryable=retryable,
        next_action=next_action,
    )
