# SPDX-License-Identifier: Apache-2.0
"""The behaviour behind every generated SDK method (planning/06 "Transport
pipeline"), written once for ``ClientCore`` and ``AsyncClientCore``::

    call -> preflight -> auth -> idempotency -> send -> classify -> verify? -> result
                                                 ^             |
                                                 +-- retries <-+

Every method is a flow (``_effects``): pure logic that yields an effect
whenever it needs I/O. The semantics are those of ``ClientCore`` in
``runtimes/ts/src/client.ts``; nothing here raises for API or transport
errors, which become diagnostic envelopes.
"""

from __future__ import annotations

import math
import random
import re
import time
import uuid
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, replace
from typing import Any, Final, Literal, cast
from urllib.parse import urljoin, urlsplit

import httpx

from ._auth import AuthMissing, AuthPlan, OAuth2Client, TokenError, is_safe_method, resolve_auth
from ._classify import (
    CallContext,
    OutcomeCheck,
    change_of,
    classify_error,
    decode_body,
    has_replay_protection,
    is_mutation,
    match_response,
    outcome_unknown,
    policy_of,
    request_id_of,
    safety_of,
    verify_callable,
)
from ._confirm import CONFIRMATION_TTL_MS, check_token, issue_token, token_expiry
from ._effects import (
    Answered,
    AttemptRequest,
    Flow,
    Lost,
    NotSent,
    TimedOut,
    emit,
    observe,
    send,
    shared,
    sleep,
    store_get,
    store_put,
)
from ._envelope import diagnostic, scrub_diagnostic, scrub_text, with_fields
from ._expr import (
    Scope,
    contains_placeholder,
    describe_predicate,
    evaluate_dry,
    evaluate_expr,
    evaluate_predicate,
    placeholders_in,
    resolve_ref,
)
from ._helpers import (
    SAFETIES,
    SAFETY_RANK,
    arg_name,
    arg_params,
    argument_path,
    body_arg,
    descriptor_problem,
    has_credentials,
    int_like,
    interpolate,
    json_ready,
    merged_fields,
    next_link,
    observable_body,
    origin_of,
    parse_retry_after,
    query_names,
    redact_below,
    safe_id,
    safe_name,
    sensitive_arg,
    sensitive_body_paths,
    sensitive_request_paths,
    sent_arguments,
    unescape_placeholders,
    values_at,
    with_arguments,
    with_search_params,
    with_wire_names,
)
from ._idempotency import check_key_format, has_key_param, key_format_description, key_header
from ._json import canonical_json, display_json, js_number
from ._serialize import (
    HeaderBag,
    SerializationError,
    body_value,
    enc,
    encode_body,
    form_text,
    scalar,
    serialize_cookie_param,
    serialize_header_param,
    serialize_path_param,
    serialize_query_param,
    valid_header_name,
    valid_header_value,
)
from ._util import (
    NOTHING,
    REDACTED,
    b64,
    bounded,
    describe_error,
    entries_of,
    envelope_value,
    field,
    get_path,
    has_cycle,
    is_array,
    is_number,
    is_record,
    items_of,
    looks_sensitive,
    normalize_files,
    random_bytes,
    redact_paths,
    redact_sensitive_keys,
    sha256_hex,
    split_path,
    str_field,
    unsupported_path,
    utf8,
    without_unset,
)
from .idempotency import MemoryIdempotencyStore
from .sentinels import UNSET
from .types import (
    ClientOptions,
    Diagnostic,
    Err,
    HttpMethod,
    Invalid,
    MacroDescriptor,
    MacroStepPreview,
    Ok,
    OperationDescriptor,
    Page,
    ParamDescriptor,
    PreviewResult,
    RenderedRequest,
    RequestContext,
    ResponseMeta,
    RetryOptions,
    Safety,
    Valid,
    Verification,
)

#: Version of ``tungsten-runtime``, sent in ``X-Tungsten-Runtime``; kept
#: equal to ``pyproject.toml`` ``version``.
RUNTIME_VERSION: Final = "0.1.0"

type Purpose = Literal["call", "preview", "server_preview", "macro_preview"]

_RETRY_CATEGORIES = frozenset({"RATE_LIMITED", "UPSTREAM_UNAVAILABLE", "TRANSPORT_FAILED", "OUTCOME_UNKNOWN"})
_DEFAULT_RETRIES: Final = {
    "max": 3,
    "base_ms": 200,
    "max_ms": 5000,
    "jitter": "full",
    "honor_retry_after": True,
}
_DEFAULT_TIMEOUT_MS: Final = 30_000
_DEFAULT_POLL_INTERVAL_MS: Final = 1000
_DEFAULT_VERIFY_BUDGET_MS: Final = 30_000
_DEFAULT_MACRO_BUDGET_MS: Final = 30_000
_MACRO_PAGE_LIMIT: Final = 100
#: Same-origin redirects followed for one read attempt.
_MAX_REDIRECTS: Final = 5
_REDIRECT_STATUSES = frozenset({301, 302, 303, 307, 308})
_EMPTY_META = ResponseMeta(status=0, headers={}, request_id=None, attempts=0)
_DOT_SEGMENT = re.compile(r"(?:\.|%2e){1,2}", re.IGNORECASE)
_PATH_FIELD = re.compile(r"\{([^{}]+)\}")
_DIGITS = re.compile(r"[0-9]+")
_ROOT_SEGMENT = re.compile(r"v[0-9]+(?:\.[0-9]+)*", re.IGNORECASE)

#: Stands in for a verification reference that did not resolve; equal to nothing.
_UNRESOLVED: Final = object()


@dataclass(frozen=True, slots=True)
class _Retries:
    max: int
    base_ms: float
    max_ms: float
    jitter: str
    honor_retry_after: bool


@dataclass(frozen=True, slots=True)
class StepControl:
    """A macro step's run: confirmed at macro level, with the hook that
    spends the macro's token once a step passed pre-flight and is about to
    be sent (a step failing pre-flight leaves the token unspent)."""

    claim: Callable[[], Err | None]


@dataclass(frozen=True, slots=True)
class Answer:
    """A call's result and the decoded wire body it came from (``UNSET`` for
    none): predicates, references, pages and macros read the wire body, the
    caller gets the validated value."""

    result: Ok[Any] | Err
    raw: object = UNSET


@dataclass(frozen=True, slots=True)
class PageAnswer:
    result: Ok[Page[Any]] | Err
    items: list[object]


@dataclass(frozen=True, slots=True)
class Prepared:
    """A request ready to send, with its redacted rendering."""

    op: OperationDescriptor
    args: dict[str, object]
    method: HttpMethod
    url: str
    display_url: str
    headers: HeaderBag
    body: bytes | None
    #: The body middleware sees: ``body``, or its redacted rendering when
    #: redaction changed a JSON or form body.
    observed_body: bytes | None
    display_body: object
    key: str | None
    key_header: str
    #: Auth query parameters, re-applied on a followed redirect.
    auth_query: list[tuple[str, str]]
    #: Wire names of query parameters whose values are shown redacted.
    hidden_query: list[str]
    secrets: frozenset[str]


def _fail(error: Diagnostic, partial: object = None) -> Err:
    return Err(error=error, partial=partial)


def _hook(middleware: object, name: str) -> object:
    try:
        return getattr(middleware, name, None)
    except Exception:
        return None


def _monotonic_ms() -> float:
    return time.monotonic() * 1000


def _opt(opts: Mapping[str, object], key: str) -> object:
    return opts.get(key, UNSET)


class Engine:
    """One per client: it owns the idempotency store, the confirmation-token
    key, the OAuth2 token cache and the registry of operations resolvable by
    id. Its public methods are flows."""

    def __init__(self, api: object, options: object) -> None:
        self.api: object = api if is_record(api) else {}
        self.options: ClientOptions = options if isinstance(options, ClientOptions) else ClientOptions()
        self.registry: dict[str, OperationDescriptor] = {}
        self.macros: dict[str, MacroDescriptor] = {}
        self._token_cache: dict[str, tuple[str, float]] = {}
        #: Confirmation tokens already used to send, with the replay
        #: protection of their first use, until they expire.
        self._used_tokens: dict[str, tuple[str | None, float]] = {}
        store: object = self.options.idempotency_store
        get, put = _hook(store, "get"), _hook(store, "put")
        self.store: object = store if callable(get) and callable(put) else MemoryIdempotencyStore()
        key = self.options.confirmation_key
        self.confirmation_key: bytes = (
            bytes(key) if isinstance(key, bytes | bytearray) and len(key) > 0 else random_bytes(32)
        )
        operations: object = self.options.operations
        if is_array(operations):
            self.register(*cast(Sequence[OperationDescriptor], operations))
        macros: object = self.options.macros
        for macro in items_of(macros):
            name = str_field(macro, "name")
            if name and name not in self.macros:
                self.macros[name] = cast(MacroDescriptor, macro)

    # ---------------------------------------------------------- registry

    def register(self, *ops: OperationDescriptor) -> None:
        """Make operations resolvable by id; invalid entries are ignored."""
        for op in ops:
            op_id = str_field(op, "id")
            if op_id and op_id not in self.registry:
                self.registry[op_id] = op

    def operation(self, operation_id: str) -> OperationDescriptor | None:
        return self.registry.get(operation_id)

    def macro(self, name: str) -> MacroDescriptor | None:
        return self.macros.get(name)

    # ------------------------------------------------------------ public

    def call(self, op: OperationDescriptor, args: object, opts: object) -> Flow[Ok[Any] | Err]:
        answer = yield from self._call(op, args, self._call_options(opts), None, None)
        return answer.result

    def preview(self, op: OperationDescriptor, args: object, opts: object) -> Flow[Ok[PreviewResult] | Err]:
        result = yield from self._preview(op, args, self._call_options(opts))
        return result

    def pages(self, op: OperationDescriptor, args: object, opts: object) -> Flow[None]:
        def deliver(page: PageAnswer) -> Flow[bool]:
            yield from emit(page.result)
            return True

        yield from self._pages(op, args, self._call_options(opts), None, deliver)

    def poll(
        self,
        op: OperationDescriptor,
        args: object,
        until: object,
        interval_ms: object,
        budget_ms: object,
        opts: object,
    ) -> Flow[Ok[Any] | Err]:
        answer = yield from self._poll(
            op, args, until, interval_ms, budget_ms, self._call_options(opts), None
        )
        return answer.result

    def run_macro(self, macro: MacroDescriptor, input: object, opts: object) -> Flow[Ok[Any] | Err]:
        result = yield from self._run_macro(macro, input, self._call_options(opts))
        return result

    def preview_macro(
        self, macro: MacroDescriptor, input: object, opts: object
    ) -> Flow[Ok[PreviewResult] | Err]:
        result = yield from self._preview_macro(macro, input, self._call_options(opts))
        return result

    def internal(self, subject: str, error: BaseException, during: str) -> Err:
        return _fail(
            diagnostic(
                subject,
                "UNEXPECTED_RESPONSE",
                remediation=(
                    f"The SDK failed while {during} ({describe_error(error)}). This is a bug in the generated SDK "
                    "or the runtime; report it. If the call may have been sent, check its effect before repeating it."
                ),
                retryable="never",
            )
        )

    # ---------------------------------------------------------- options

    def _call_options(self, opts: object) -> dict[str, object]:
        return dict(entries_of(opts))

    def now(self) -> float:
        clock = self.options.now
        if callable(clock):
            value = clock()
            if is_number(value) and math.isfinite(value):
                return float(value)
        return time.time() * 1000

    def _retry_options(self, op: OperationDescriptor) -> _Retries:
        """The runtime defaults, then the API's defaults for the operation's
        tier, then ``ClientOptions.retries``; each layer overrides the fields
        it sets."""
        tier = field(field(self.api, "retries"), "mutating" if is_mutation(op) else "read_only")
        configured: object = self.options.retries
        client: Mapping[str, object]
        if isinstance(configured, RetryOptions):
            client = {
                "max": configured.max,
                "base_ms": configured.base_ms,
                "max_ms": configured.max_ms,
                "jitter": configured.jitter,
                "honor_retry_after": configured.honor_retry_after,
            }
        else:
            client = configured if is_record(configured) else {}
        layers: list[Mapping[str, object]] = [tier if is_record(tier) else {}, client]

        def pick(key: str, valid: Callable[[object], bool]) -> object:
            value = _DEFAULT_RETRIES[key]
            for layer in layers:
                candidate = layer.get(key, UNSET)
                if valid(candidate):
                    value = candidate
            return value

        def finite(v: object) -> bool:
            return is_number(v) and math.isfinite(v)

        return _Retries(
            max=math.floor(bounded(pick("max", finite), 3, 0, 100)),
            base_ms=bounded(pick("base_ms", finite), 200, 0),
            max_ms=bounded(pick("max_ms", finite), 5000, 0),
            jitter=str(pick("jitter", lambda v: v in ("none", "equal", "full"))),
            honor_retry_after=pick("honor_retry_after", lambda v: isinstance(v, bool)) is True,
        )

    def _timeout(self, opts: Mapping[str, object]) -> float:
        return bounded(_opt(opts, "timeout_ms"), bounded(self.options.timeout_ms, _DEFAULT_TIMEOUT_MS, 1), 1)

    def _middleware(self) -> list[object]:
        middleware: object = self.options.middleware
        return [m for m in middleware if m is not None] if is_array(middleware) else []

    def _emit(self, d: Diagnostic) -> Flow[None]:
        yield from observe(self.options.on_diagnostic, d)

    # -------------------------------------------------------------- call

    def _call(
        self,
        op: OperationDescriptor,
        args: object,
        opts: Mapping[str, object],
        url_override: str | None,
        step: StepControl | None,
    ) -> Flow[Answer]:
        prepared = yield from self._prepare(op, args, opts, "call", url_override, step)
        if isinstance(prepared, Err):
            return Answer(prepared)
        answer = yield from self._send(prepared, opts)
        if (
            isinstance(answer.result, Ok)
            and _opt(opts, "verify") is True
            and is_record(field(op["agent"], "verify"))
        ):
            verification = yield from self._verify(op, prepared.args, answer.raw, opts)
            return Answer(replace(answer.result, verification=verification), answer.raw)
        return answer

    # --------------------------------------------------------- preflight

    def _validation(
        self,
        op: OperationDescriptor,
        path: Sequence[str | int],
        value: object,
        expected: str,
        remediation: str,
    ) -> Err:
        return _fail(
            diagnostic(
                op["id"],
                "VALIDATION_FAILED",
                failed_parameter=argument_path(op, path),
                received_value=envelope_value(redact_below(op, path, value), sensitive_arg(op, path)),
                expected=expected,
                remediation=remediation,
            )
        )

    def _validate_args(
        self, op: OperationDescriptor, args: dict[str, object], placeholders: bool
    ) -> Err | dict[str, object]:
        """Pre-flight validation of the args; the validated args on success.
        With ``placeholders``, an issue at (or below) a value that is a dry
        evaluation's placeholder is not a failure."""
        params = arg_params(op)
        op_id = op["id"]
        for p in params:
            if p.get("required") is True and args.get(p["name"]) is None:
                return self._validation(
                    op,
                    [p["name"]],
                    args.get(p["name"], UNSET),
                    f"a value for the required {p['location']} parameter {p['wire']}",
                    f"Pass {p['name']}; {op_id} cannot be called without it.",
                )
        body = op["body"]
        arg = body_arg(op)
        if (
            body is not None
            and body.get("required") is True
            and arg is not None
            and args.get(arg, UNSET) is UNSET
        ):
            return self._validation(op, [arg], UNSET, "a request body", f"Pass the request body as {arg}.")
        validate = _hook(op.get("request"), "validate")
        if callable(validate):
            outcome = validate(args)
            if isinstance(outcome, Valid):
                data: object = cast(Valid[object], outcome).data
                return dict(data) if is_record(data) else args
            if not isinstance(outcome, Invalid):
                raise TypeError("the request validator returned neither Valid nor Invalid")
            issues = items_of(cast(object, outcome.issues))

            def path_of(issue: object) -> list[str | int]:
                path = field(issue, "path")
                if not is_array(path):
                    return []
                return [s for s in path if isinstance(s, str | int) and not isinstance(s, bool)]

            def pending(path: list[str | int]) -> bool:
                return placeholders and any(
                    contains_placeholder(get_path(args, [str(s) for s in path[: i + 1]]))
                    for i in range(len(path))
                )

            issue: object = None
            if placeholders:
                issue = next((i for i in issues if not pending(path_of(i))), None)
                if issue is None:
                    return args
            elif len(issues) > 0:
                issue = issues[0]
            path = path_of(issue)
            message = str_field(issue, "message") or "a valid value"
            return self._validation(
                op,
                path,
                get_path(args, [str(s) for s in path]),
                message,
                f"Fix {argument_path(op, path)} ({message}) and call again.",
            )
        # Without a validator, unknown keys are rejected so a misspelt
        # argument is never silently dropped (rpc methods without a body
        # take free-form args).
        if op["rpc"] is not None and body is None:
            return args
        allowed = {p["name"] for p in params} | {a for a, _ in merged_fields(op)}
        if arg is not None:
            allowed.add(arg)
        for key, value in args.items():
            if key not in allowed and value is not UNSET:
                names = sorted(allowed)
                listed = ", ".join(names[:20]) if len(names) > 0 else "no arguments"
                return self._validation(
                    op, [key], value, f"one of: {listed}", f"Remove {key}; {op_id} does not accept it."
                )
        return args

    def _check_key(self, op: OperationDescriptor, opts: Mapping[str, object], purpose: Purpose) -> Err | None:
        policy = op["agent"]["idempotency"]
        kind = policy.get("policy")
        fmt = str_field(policy, "format")
        key = _opt(opts, "idempotency_key")
        note_text = str_field(policy, "note")
        note = f" {note_text}" if note_text else ""
        op_id = op["id"]
        if key is UNSET or key is None:
            if purpose == "call" and kind == "caller_owned" and policy.get("persist_required") is True:
                fresh = "UUIDv4" if re.search("uuid", fmt or "", re.IGNORECASE) else "unique key"
                return _fail(
                    diagnostic(
                        op_id,
                        "VALIDATION_FAILED",
                        failed_parameter="idempotencyKey",
                        expected=key_format_description(fmt),
                        remediation=(
                            f"Generate a random {fresh}, persist it with the intent of this {op_id} call, and pass "
                            "it as idempotency_key. Reuse the exact same key on every retry; a new key can apply "
                            f"the effect twice.{note}"
                        ),
                    )
                )
            return None
        if kind == "none" and not has_key_param(op):
            return None
        if kind in ("content_identity", "content_hash"):
            return None
        expected = (
            check_key_format(key, fmt if kind == "caller_owned" else None)
            if isinstance(key, str)
            else key_format_description(fmt)
        )
        if expected is not None:
            return _fail(
                diagnostic(
                    op_id,
                    "VALIDATION_FAILED",
                    failed_parameter="idempotencyKey",
                    received_value=envelope_value(key, False),
                    expected=expected,
                    remediation=(
                        f"Pass idempotency_key as {expected}. Generate it once, persist it with the intent of this "
                        f"call, and reuse it on every retry.{note}"
                    ),
                )
            )
        return None

    def _confirmed(
        self,
        subject_id: str,
        subject: str,
        safety: object,
        args: object,
        confirm: object,
        preview_call: str,
        allow_true: bool,
    ) -> Err | None:
        """The confirmation rule of planning/04 for one tier: ``destructive``
        accepts True (unless ``allow_true`` is False) or a token,
        ``irreversible`` only a token issued for ``subject`` (an operation id,
        or ``macro:<name>``) and these exact args."""
        if safety not in ("destructive", "irreversible"):
            return None
        how = f"call {preview_call} with the same arguments and pass its confirmation_token as confirm"

        def required(remediation: str) -> Err:
            return _fail(
                diagnostic(
                    subject_id,
                    "CONFIRMATION_REQUIRED",
                    failed_parameter="confirm",
                    expected="a confirmation_token from preview()",
                    remediation=remediation,
                )
            )

        if confirm is True:
            if safety == "destructive" and allow_true:
                return None
            return required(f"{subject_id} is {safety}, so confirm=True is not accepted: {how}.")
        if not isinstance(confirm, str) or confirm == "":
            offer = " (or pass confirm=True)" if safety == "destructive" and allow_true else ""
            return required(f"{subject_id} is {safety}: {how}{offer}.")
        check = check_token(self.confirmation_key, confirm, subject, args, self.now())
        if check == "valid":
            return None
        if check == "expired":
            return required(
                f"The confirmation token expired (tokens last {CONFIRMATION_TTL_MS // 60000} minutes): {how}."
            )
        return required(
            f"The confirmation token was not issued by this client for {subject_id} with exactly these arguments: {how}."
        )

    def _claim_token(self, subject_id: str, token: str, bind: str | None, preview_call: str) -> Err | None:
        """Spend a valid confirmation token on the call about to be sent: its
        first use binds it to that call's replay protection, and a later use
        is accepted only as a retry under the same protection. A call without
        protection can use a token once."""
        now = self.now()
        for used, (_, expiry) in list(self._used_tokens.items()):
            if expiry <= now:
                del self._used_tokens[used]
        previous = self._used_tokens.get(token)
        if previous is not None:
            previous_bind = previous[0]
            if previous_bind is not None and previous_bind == bind:
                return None
            if previous_bind is None:
                why = "This call has no idempotency key, so a repeat can apply the effect twice"
            else:
                other = "an idempotency key" if bind is None else "another idempotency key"
                why = f"It was used with {other}; only a retry with that same key may reuse it"
            return _fail(
                diagnostic(
                    subject_id,
                    "CONFIRMATION_REQUIRED",
                    failed_parameter="confirm",
                    expected="a confirmation_token from preview()",
                    remediation=(
                        f"This confirmation token was already used for one {subject_id} call. {why}. Check whether "
                        f"that call took effect; to send again, call {preview_call} and confirm with its new token."
                    ),
                )
            )
        expiry = token_expiry(token)
        self._used_tokens[token] = (bind, float(expiry) if expiry is not None else now + CONFIRMATION_TTL_MS)
        return None

    # ---------------------------------------------------------- building

    def _prepare(
        self,
        op: OperationDescriptor,
        raw_args: object,
        opts: Mapping[str, object],
        purpose: Purpose,
        url_override: str | None,
        step: StepControl | None,
    ) -> Flow[Prepared | Err]:
        problem = descriptor_problem(op)
        if problem is not None:
            return _fail(
                diagnostic(
                    safe_id(op),
                    "VALIDATION_FAILED",
                    failed_parameter="operation",
                    expected="a valid operation descriptor",
                    remediation=(
                        f"The operation descriptor is invalid: {problem}. Regenerate the SDK or fix the descriptor; "
                        "nothing was sent."
                    ),
                )
            )
        self.register(op)
        op_id = op["id"]
        if not is_record(raw_args):
            example = ", ".join(f'"{p["name"]}": ...' for p in arg_params(op)[:3])
            return _fail(
                diagnostic(
                    op_id,
                    "VALIDATION_FAILED",
                    failed_parameter="args",
                    received_value=envelope_value(raw_args, False),
                    expected="a mapping of named arguments",
                    remediation=f"Pass the arguments of {op_id} as one mapping, e.g. {{{example}}}.",
                )
            )
        if has_cycle(raw_args):
            return self._validation(
                op,
                [],
                "<cyclic value>",
                "a JSON-like value without cycles",
                "Pass arguments without circular references.",
            )
        checked = self._validate_args(
            op, cast(dict[str, object], without_unset(raw_args)), purpose == "macro_preview"
        )
        if isinstance(checked, Err):
            return checked
        args = cast(dict[str, object], normalize_files(checked))
        unsupported = unsupported_path(args)
        if unsupported is not None:
            return self._validation(
                op,
                unsupported,
                get_path(args, [str(s) for s in unsupported]),
                "JSON-like data (str, int, float, bool, None, a list or a mapping) or binary data",
                f"Pass {argument_path(op, unsupported)} as JSON-like data; it cannot be sent.",
            )
        invalid_key = self._check_key(op, opts, purpose)
        if invalid_key is not None:
            return invalid_key
        agent = op["agent"]
        safety = agent["safety"]
        if purpose == "call" and step is None:
            unconfirmed = self._confirmed(
                op_id,
                op_id,
                safety,
                args,
                _opt(opts, "confirm"),
                "preview(...)",
                _opt(opts, "allow_confirm_true") is not False,
            )
            if unconfirmed is not None:
                return unconfirmed

        method = op["method"]
        configured_auth: object = self.options.auth
        auth = yield from resolve_auth(
            field(self.api, "auth"),
            op_id,
            op["security"],
            entries_of(configured_auth),
            method,
            self._token_source,
        )
        if isinstance(auth, AuthMissing):
            return _fail(diagnostic(op_id, "AUTH_FAILED", remediation=auth.remediation))
        secrets = {s for s in auth.secrets if s != ""}
        for p in op["params"]:
            value = args.get(p["name"])
            if (
                p.get("sensitive") is True
                and isinstance(value, str | int | float)
                and not isinstance(value, bool)
            ):
                secrets.add(scalar(value))
        for path in sensitive_request_paths(op):
            found: list[str] = []
            values_at(args, split_path(path), found)
            secrets.update(found)

        url = self._build_url(op, args, auth.plan, url_override)
        if isinstance(url, Err):
            return url

        headers = HeaderBag()
        self._build_headers(op, args, opts, auth.plan, headers)

        body = op["body"]
        try:
            value = (
                UNSET
                if is_safe_method(method) and method != "OPTIONS"
                else body_value(op, args, {p["name"] for p in arg_params(op)})
            )
            body_paths = sensitive_body_paths(op)

            def redact(display: object) -> object:
                if "" in body_paths:
                    return redact_sensitive_keys(REDACTED)
                return redact_sensitive_keys(redact_paths(display, [p for p in body_paths if p != ""]))

            encoded = encode_body(body, value, redact)
        except SerializationError as error:
            arg = body_arg(op)
            at: list[str | int] = [arg] if error.parameter == "body" and arg is not None else []
            return self._validation(
                op, at, error.value, error.expected, f"Pass {error.parameter} as {error.expected}."
            )
        if encoded.content_type:
            headers.set("Content-Type", encoded.content_type)
        observed_body = observable_body(body["encoding"] if body is not None else "json", encoded)

        header = key_header(op)
        key = yield from self._idempotency_key(op, args, opts, purpose, encoded.hash_material)
        if isinstance(key, Err):
            return key
        policy = policy_of(op)
        if key is not None:
            headers.set(header, key, True)
            secrets.add(key)
        elif purpose != "call" and policy in ("auto", "caller_owned"):
            headers.set(header, "<set at call time>")
        for h in auth.plan.headers:
            headers.set(h.name, h.value, h.secret)
        preview_mode = agent["preview"]
        if purpose == "server_preview" and preview_mode.get("mode") == "header":
            dry_header = str_field(preview_mode, "header")
            dry_value = str_field(preview_mode, "value")
            if dry_header is not None and dry_value is not None:
                headers.set(dry_header, dry_value)

        for name, header_value in headers.to_record(False).items():
            if not valid_header_name(name) or not valid_header_value(header_value):
                if any(h.name.lower() == name.lower() for h in auth.plan.headers):
                    return _fail(
                        diagnostic(
                            op_id,
                            "AUTH_FAILED",
                            remediation=(
                                f"The credential for header {name} contains characters not allowed in an HTTP header; "
                                "check ClientOptions.auth."
                            ),
                        )
                    )
                param = next((p for p in op["params"] if p["wire"].lower() == name.lower()), None)
                return self._validation(
                    op,
                    [param["name"]] if param is not None else [],
                    REDACTED if headers.is_secret(name) else header_value,
                    "a header value of visible ASCII characters (no line breaks)",
                    f"Header {name} cannot carry this value; remove line breaks and non-Latin-1 characters.",
                )
        confirm = _opt(opts, "confirm")
        if purpose == "call" and step is not None:
            spent = step.claim()
            if spent is not None:
                return spent
        elif purpose == "call" and isinstance(confirm, str) and safety in ("destructive", "irreversible"):
            bind = key if key is not None else "content-identity" if policy == "content_identity" else None
            spent = self._claim_token(op_id, confirm, bind, "preview(...)")
            if spent is not None:
                return spent
        hidden = [name for name, _ in auth.plan.query] + [
            p["wire"] for p in op["params"] if p["location"] == "query" and p.get("sensitive") is True
        ]
        return Prepared(
            op=op,
            args=args,
            method=method,
            url=url[0],
            display_url=url[1],
            headers=headers,
            body=encoded.body,
            observed_body=observed_body,
            display_body=encoded.display,
            key=key,
            key_header=header,
            auth_query=list(auth.plan.query),
            hidden_query=hidden,
            secrets=frozenset(secrets | set(headers.secrets())),
        )

    def _base_url(self) -> str | None:
        configured: object = self.options.base_url
        if isinstance(configured, str):
            return configured
        servers = field(self.api, "servers")
        listed = [s for s in servers if isinstance(s, str)] if is_array(servers) else []
        return listed[0] if len(listed) > 0 else None

    def _build_url(
        self, op: OperationDescriptor, args: Mapping[str, object], plan: AuthPlan, url_override: str | None
    ) -> tuple[str, str] | Err:
        op_id = op["id"]
        base = self._base_url()
        if not base:
            return _fail(
                diagnostic(
                    op_id,
                    "TRANSPORT_FAILED",
                    retryable="never",
                    remediation="No base URL is configured: set ClientOptions.base_url. Nothing was sent.",
                )
            )
        params = arg_params(op)
        if url_override is not None:
            url = url_override
            display = url_override
            if len(plan.query) > 0:
                url = with_search_params(url, plan.query, True)
                display = with_search_params(display, [(name, REDACTED) for name, _ in plan.query], False)
        else:
            unknown: list[str] = []
            missing: list[str] = []

            def path_param(wire: str) -> ParamDescriptor | None:
                for p in params:
                    if p["location"] == "path" and p["wire"] == wire:
                        return p
                for p in params:
                    if p["location"] == "path" and p["name"] == wire:
                        return p
                return None

            def substitute(match: re.Match[str]) -> str:
                p = path_param(match.group(1))
                if p is None:
                    unknown.append(match.group(1))
                    return match.group(0)
                value = args.get(p["name"])
                if value is None:
                    missing.append(p["name"])
                    return match.group(0)
                return serialize_path_param(p, value)

            template = op["path"]
            path = _PATH_FIELD.sub(substitute, template)
            if len(unknown) > 0:
                return _fail(
                    diagnostic(
                        op_id,
                        "VALIDATION_FAILED",
                        failed_parameter="operation",
                        expected="a valid operation descriptor",
                        remediation=(
                            f"The path template has a placeholder {{{unknown[0]}}} with no path parameter; "
                            "regenerate the SDK."
                        ),
                    )
                )
            if len(missing) > 0:
                p = path_param(missing[0])
                wire = p["wire"] if p is not None else missing[0]
                return self._validation(
                    op, [missing[0]], UNSET, f"a value for path parameter {wire}", f"Pass {missing[0]}."
                )
            # A path segment that is empty or a dot segment (also
            # percent-encoded) would be removed or resolved by URL parsing,
            # so the request would reach another resource: refuse it before
            # anything is built or confirmed.
            template_segments = template.split("/")
            built_segments = path.split("/")
            if len(template_segments) == len(built_segments):
                for index, segment in enumerate(built_segments):
                    segment_template = template_segments[index]
                    if "{" not in segment_template or not (segment == "" or _DOT_SEGMENT.fullmatch(segment)):
                        continue
                    found = _PATH_FIELD.search(segment_template)
                    p = path_param(found.group(1)) if found is not None else None
                    name = p["name"] if p is not None else None
                    return self._validation(
                        op,
                        [name] if name is not None else [],
                        args.get(name, UNSET) if name is not None else segment,
                        "a non-empty path segment other than . and ..",
                        f"Pass {name if name is not None else 'the path parameter'} as the identifier of one resource; "
                        f'"{segment}" would change the request path.',
                    )
            parts: list[str] = []
            shown: list[str] = []
            for p in params:
                if p["location"] != "query":
                    continue
                serialized = serialize_query_param(p, args.get(p["name"], UNSET))
                parts.extend(serialized)
                shown.extend(
                    [f"{s.split('=')[0]}={REDACTED}" for s in serialized]
                    if p.get("sensitive") is True
                    else serialized
                )
            for name, value in plan.query:
                parts.append(f"{enc(name)}={enc(value)}")
                shown.append(f"{enc(name)}={REDACTED}")
            root = base.rstrip("/")

            def join(listed: list[str]) -> str:
                return f"{'&' if '?' in path else '?'}{'&'.join(listed)}" if len(listed) > 0 else ""

            url = f"{root}{path}{join(parts)}"
            display = f"{root}{path}{join(shown)}"
        if not _valid_url(url):
            return _fail(
                diagnostic(
                    op_id,
                    "TRANSPORT_FAILED",
                    retryable="never",
                    remediation=(
                        "The base URL is not a valid absolute http(s) URL without credentials: fix "
                        "ClientOptions.base_url. Nothing was sent."
                    ),
                )
            )
        return url, display

    def _build_headers(
        self,
        op: OperationDescriptor,
        args: Mapping[str, object],
        opts: Mapping[str, object],
        plan: AuthPlan,
        headers: HeaderBag,
    ) -> None:
        accept = list(
            dict.fromkeys(
                str(r.get("media_type"))
                for r in op["responses"]
                if is_record(r) and r.get("kind") == "success" and isinstance(r.get("media_type"), str)
            )
        )
        if len(accept) > 0:
            headers.set("Accept", ", ".join(accept))
        api_name = str_field(self.api, "name") or "api"
        api_version = str_field(self.api, "version") or "0"
        headers.set("X-Tungsten-Runtime", f"tungsten-py/{RUNTIME_VERSION} {api_name}-sdk/{api_version}")
        headers.set("X-Tungsten-Operation", op["id"])
        tungsten = str_field(self.api, "tungsten_version") or RUNTIME_VERSION
        headers.set("User-Agent", f"{api_name}-sdk/{api_version} tungsten/{tungsten} (python)")
        for extra in (cast(object, self.options.headers), _opt(opts, "headers")):
            if not is_record(extra):
                continue
            for name, value in extra.items():
                if isinstance(value, str):
                    headers.set(name, value, looks_sensitive(name))
        cookies: list[str] = []
        existing = headers.get("Cookie")
        if existing is not None:
            cookies.append(existing)
        for p in arg_params(op):
            value = args.get(p["name"])
            if value is None:
                continue
            if p["location"] == "header":
                headers.set(p["wire"], serialize_header_param(p, value), p.get("sensitive") is True)
            elif p["location"] == "cookie":
                cookies.append(serialize_cookie_param(p, value))
        cookies.extend(f"{name}={value}" for name, value in plan.cookies)
        # Cookies are session material: the whole header is always redacted.
        if len(cookies) > 0:
            headers.set("Cookie", "; ".join(cookies), True)

    def _idempotency_key(
        self,
        op: OperationDescriptor,
        args: Mapping[str, object],
        opts: Mapping[str, object],
        purpose: Purpose,
        hash_material: str | bytes | None,
    ) -> Flow[str | Err | None]:
        policy = policy_of(op)
        given = _opt(opts, "idempotency_key")
        supplied = given if isinstance(given, str) else None
        if policy == "content_identity":
            return None
        if policy == "content_hash":
            return None if hash_material is None else sha256_hex(hash_material)
        if policy == "caller_owned":
            return supplied
        if policy == "none":
            return supplied if has_key_param(op) else None
        if supplied is not None:
            return supplied
        if purpose != "call":
            return None
        logical_id = sha256_hex(canonical_json(args))
        try:
            existing = yield from store_get(self.store, op["id"], logical_id)
            if isinstance(existing, str) and existing != "":
                return existing
            key = str(uuid.uuid4())
            yield from store_put(self.store, op["id"], logical_id, key)
            return key
        except Exception as error:
            return _fail(
                diagnostic(
                    op["id"],
                    "TRANSPORT_FAILED",
                    retryable="never",
                    remediation=(
                        f"The idempotency store failed ({describe_error(error)}), so the call was not sent rather "
                        "than risk a second key for the same intent. Fix or replace ClientOptions.idempotency_store."
                    ),
                )
            )

    def _token_source(self, client: OAuth2Client) -> Flow[str]:
        cached = self._token_cache.get(client.scheme)
        if cached is not None and cached[1] > time.time() * 1000 + 30_000:
            return cached[0]
        token = yield from shared(f"oauth2:{client.scheme}", lambda: self._fetch_token(client))
        return token

    def _fetch_token(self, client: OAuth2Client) -> Flow[str]:
        name = client.scheme
        form = [("grant_type", "client_credentials")]
        if len(client.scopes) > 0:
            form.append(("scope", " ".join(client.scopes)))
        basic = b64(utf8(f"{enc(client.client_id)}:{enc(client.client_secret)}"))
        outcome = yield from send(
            AttemptRequest(
                url=client.token_url,
                method="POST",
                headers={
                    "Content-Type": "application/x-www-form-urlencoded",
                    "Accept": "application/json",
                    "Authorization": f"Basic {basic}",
                },
                body=form_text(form).encode("ascii"),
                timeout_ms=bounded(self.options.timeout_ms, _DEFAULT_TIMEOUT_MS, 1),
            )
        )
        if not isinstance(outcome, Answered) or outcome.body is None:
            raise TokenError(
                f"The OAuth2 token endpoint for {name} could not be reached; check network access and the token URL."
            )
        if not 200 <= outcome.status <= 299:
            raise TokenError(
                f"The OAuth2 token endpoint for {name} answered HTTP {outcome.status}; check client_id, "
                "client_secret and scopes."
            )
        decoded = decode_body(outcome.body, outcome.headers, "application/json")
        token = get_path(decoded.value, "access_token")
        if not isinstance(token, str) or token == "":
            raise TokenError(f"The OAuth2 token endpoint for {name} returned no access_token.")
        expires_in = get_path(decoded.value, "expires_in")
        lifetime = (
            float(expires_in) * 1000 if is_number(expires_in) and math.isfinite(expires_in) else 3_600_000
        )
        self._token_cache[name] = (token, time.time() * 1000 + lifetime)
        return token

    # ----------------------------------------------------------- sending

    def _send(self, prepared: Prepared, opts: Mapping[str, object]) -> Flow[Answer]:
        op = prepared.op
        retries = self._retry_options(op)
        mutation = is_mutation(op)
        protected = has_replay_protection(op, prepared.key)
        check = self._outcome_check(op, prepared.args) if mutation and not protected else None
        timeout_ms = self._timeout(opts)
        attempts = 0
        while True:
            attempts += 1
            ctx = RequestContext(
                operation=op,
                attempt=attempts,
                method=prepared.method,
                url=prepared.display_url,
                headers=prepared.headers.to_record(True),
                body=prepared.observed_body,
            )
            headers = yield from self._before_request(ctx, prepared)
            # Redirects are never followed by the HTTP client: it would forward
            # credential headers to another origin. A read follows same-origin
            # redirects here; a mutation follows none.
            outcome = yield from send(
                AttemptRequest(prepared.url, prepared.method, headers, prepared.body, timeout_ms)
            )
            current_url, current_method, current_body = prepared.url, prepared.method, prepared.body
            hops = 0
            while (
                not mutation
                and hops < _MAX_REDIRECTS
                and isinstance(outcome, Answered)
                and outcome.status in _REDIRECT_STATUSES
            ):
                target = self._redirect_target(prepared, current_url, outcome.headers.get("location"))
                if target is None:
                    break
                rewrite = outcome.status not in (307, 308) and current_method not in ("GET", "HEAD")
                current_url = target[0]
                current_method = "GET" if rewrite else current_method
                current_body = None if rewrite else current_body
                ctx.url = target[1]
                ctx.method = current_method
                hop_headers = {
                    k: v for k, v in headers.items() if not (rewrite and k.lower() == "content-type")
                }
                outcome = yield from send(
                    AttemptRequest(current_url, current_method, hop_headers, current_body, timeout_ms)
                )
                hops += 1
            call_ctx = CallContext(self.api, op, prepared.key, prepared.key_header, attempts, check)
            answer = yield from self._classify(call_ctx, prepared, outcome, ctx, timeout_ms)
            result = answer.result
            if isinstance(result, Ok):
                return answer
            error = scrub_diagnostic(result.error, prepared.secrets)
            done = Answer(_fail(error, result.partial), answer.raw)
            if attempts > retries.max or error["category"] not in _RETRY_CATEGORIES:
                return done
            if error["retryable"] in ("never", "after_remediation"):
                return done
            if mutation and not protected:
                return done
            retry_after = error["retry_after_ms"]
            if retries.honor_retry_after and retry_after is not None:
                if retry_after > retries.max_ms:
                    return done
                delay = float(retry_after)
            else:
                ceiling = min(retries.max_ms, retries.base_ms * 2 ** (attempts - 1))
                source = self.options.random
                rnd = bounded(source(), 0.5, 0, 1) if callable(source) else random.random()
                if retries.jitter == "none":
                    delay = ceiling
                elif retries.jitter == "equal":
                    delay = ceiling / 2 + rnd * ceiling / 2
                else:
                    delay = rnd * ceiling
            for m in self._middleware():
                yield from observe(_hook(m, "on_retry"), ctx, error)
            yield from sleep(delay)

    def _outcome_check(self, op: OperationDescriptor, args: Mapping[str, object]) -> OutcomeCheck | None:
        """How to find out whether ``op`` (a mutation without replay
        protection) took effect after its answer was lost: its verification
        hook when it can be called without the lost response, else a
        registered read of the same resource."""
        sent = sent_arguments(op, args)
        hook = field(op["agent"], "verify")
        target = str_field(hook, "operation")
        if target is not None and verify_callable(hook):
            scope: Scope = {"args": with_wire_names(op, args)}
            hook_args = field(hook, "args")
            evaluated = evaluate_expr({} if hook_args is UNSET or hook_args is None else hook_args, scope)
            call = f"{target}{with_arguments(evaluated)}"
            # `expect` is what a successful call leaves behind; references to
            # the call's arguments are known, references to its response not.
            lost: list[str] = []

            def resolve(node: object, depth: int = 0) -> object:
                if depth > 32:
                    return None
                if isinstance(node, str) and node.startswith("$response"):
                    rest = node[len("$response") :]
                    lost.append((rest[1:] if rest.startswith(".") else rest) or "body")
                    return f"<{node[1:]}>"
                if isinstance(node, str) and node.startswith("$"):
                    value = resolve_ref(node, scope)
                    return f"<{node[1:]}>" if value is UNSET or value is None else value
                if is_array(node):
                    return [resolve(item, depth + 1) for item in node]
                if is_record(node):
                    return {k: resolve(v, depth + 1) for k, v in node.items()}
                return node

            expect_value = field(hook, "expect")
            expect: object = resolve(expect_value) if is_record(expect_value) else NOTHING
            fields = list(entries_of(expect))
            if len(fields) == 0:
                shows = f"whether it shows {change_of(op)}"
            elif len(lost) > 0:
                shows = (
                    f"whether {' and '.join(fields)} holds what {op['id']} creates, matching the arguments you sent"
                    f"{sent} (its {', '.join(dict.fromkeys(lost))} was in the lost response)"
                )
            else:
                shows = f"whether {describe_predicate(expect)}"
            return OutcomeCheck(call, shows)
        read = self._resource_read(op, args)
        if read is None:
            return None
        return OutcomeCheck(f"{read[0]}{with_arguments(read[1])}", f"whether it shows {change_of(op)}")

    def _resource_read(
        self, op: OperationDescriptor, args: Mapping[str, object]
    ) -> tuple[str, dict[str, object]] | None:
        """A registered read of the resource ``op`` changes: a GET whose path is
        the longest prefix of ``op``'s path, never the API root, whose
        required parameters are all path parameters ``op`` was called with.
        Candidates on one path are taken in id order."""
        if op["rpc"] is not None:
            return None
        known: dict[str, object] = {}
        for p in op["params"]:
            if p["location"] == "path" and args.get(p["name"]) is not None:
                known[p["wire"]] = args[p["name"]]
        segments = op["path"].split("/")

        def root(segment: str) -> bool:
            return segment in ("", "api") or _ROOT_SEGMENT.fullmatch(segment) is not None

        reads = sorted(
            (
                r
                for r in self.registry.values()
                if r is not op
                and descriptor_problem(r) is None
                and r["method"] == "GET"
                and safety_of(r) == "read_only"
                and r["rpc"] is None
            ),
            key=lambda r: r["id"],
        )
        for end in range(len(segments), 0, -1):
            prefix = segments[:end]
            if all(root(s) for s in prefix):
                break
            path = "/".join(prefix)
            for read in reads:
                if read["path"] != path:
                    continue
                params = arg_params(read)
                if not all(
                    p["location"] == "path" and p["wire"] in known
                    for p in params
                    if p.get("required") is True
                ):
                    continue
                read_args = {
                    p["wire"]: known[p["wire"]]
                    for p in params
                    if p["location"] == "path" and p["wire"] in known
                }
                return read["id"], read_args
        return None

    def _redirect_target(
        self, prepared: Prepared, current: str, location: str | None
    ) -> tuple[str, str] | None:
        """The URL a read's redirect points to, when it stays on the request's
        origin, with the auth query re-applied; None for any other origin."""
        if not location:
            return None
        origin = origin_of(current)
        try:
            target = urljoin(current, location)
        except ValueError:
            return None
        if origin is None or origin_of(target) != origin or has_credentials(target):
            return None
        if len(prepared.auth_query) > 0:
            target = with_search_params(target, prepared.auth_query, True)
        shown = target
        hidden = [(name, REDACTED) for name in prepared.hidden_query if name in query_names(target)]
        if len(hidden) > 0:
            shown = with_search_params(target, hidden, False)
        return target, shown

    def _before_request(self, ctx: RequestContext, prepared: Prepared) -> Flow[dict[str, str]]:
        """Run ``on_request`` with redacted headers; headers a middleware adds
        or changes (other than redacted ones) are applied to the request."""
        real = prepared.headers.to_record(False)
        shown = dict(ctx.headers)
        for m in self._middleware():
            yield from observe(_hook(m, "on_request"), ctx)
        seen: object = ctx.headers
        if not is_record(seen):
            return real
        lower = {k.lower(): k for k in real}
        for name, value in list(seen.items()):
            if not isinstance(value, str) or value == REDACTED or shown.get(name) == value:
                continue
            if not valid_header_name(name) or not valid_header_value(value):
                continue
            existing = lower.get(name.lower())
            if existing is not None and prepared.headers.is_secret(existing):
                continue
            if existing is not None:
                del real[existing]
            real[name] = value
        return real

    def _after_response(
        self, ctx: RequestContext, prepared: Prepared, status: int, headers: Mapping[str, str]
    ) -> Flow[None]:
        hooks = [h for h in (_hook(m, "on_response") for m in self._middleware()) if callable(h)]
        if len(hooks) == 0:
            return
        shown = {k: scrub_text(v, prepared.secrets) for k, v in headers.items()}
        for hook in hooks:
            yield from observe(hook, ctx, status, dict(shown))

    def _classify(
        self,
        call_ctx: CallContext,
        prepared: Prepared,
        outcome: Answered | NotSent | Lost | TimedOut,
        ctx: RequestContext,
        timeout_ms: float,
    ) -> Flow[Answer]:
        op = prepared.op
        op_id = op["id"]
        mutation = is_mutation(op)
        attempts = call_ctx.attempts
        waited = js_number(timeout_ms)
        if isinstance(outcome, NotSent):
            return Answer(
                _fail(
                    diagnostic(
                        op_id,
                        "TRANSPORT_FAILED",
                        remediation=(
                            f"The request could not be delivered ({outcome.detail}), so the server did not receive "
                            "it. Check base_url and network access, then call again."
                        ),
                        attempts=attempts,
                    )
                )
            )
        if isinstance(outcome, Lost):
            cause = f"The connection failed before a response arrived ({outcome.detail})."
            return Answer(
                _fail(
                    outcome_unknown(call_ctx, cause)
                    if mutation
                    else diagnostic(
                        op_id,
                        "TRANSPORT_FAILED",
                        remediation=f"{cause} This read has no side effects; call again.",
                        attempts=attempts,
                    )
                )
            )
        if isinstance(outcome, TimedOut):
            cause = f"No response arrived within {waited} ms."
            return Answer(
                _fail(
                    outcome_unknown(call_ctx, cause)
                    if mutation
                    else diagnostic(
                        op_id,
                        "UPSTREAM_UNAVAILABLE",
                        remediation=(
                            f"{cause} This read has no side effects; call again later or with a larger timeout_ms."
                        ),
                        attempts=attempts,
                    )
                )
            )
        status, headers = outcome.status, outcome.headers
        yield from self._after_response(ctx, prepared, status, headers)
        request_id = request_id_of(headers)
        retry_after = parse_retry_after(headers.get("retry-after"), self.now())
        declared = match_response(op["responses"], status)
        declared_media = str_field(declared, "media_type")
        success = 200 <= status <= 299 or (
            declared is not None and declared.get("kind") == "success" and 100 <= status <= 399
        )
        verify_target = str_field(field(op["agent"], "verify"), "operation")
        hint = f"Call {verify_target} to read the current state." if verify_target is not None else None

        if success and outcome.body is None:
            failure = outcome.body_failure or "broken"
            if not mutation:
                return Answer(
                    _fail(
                        diagnostic(
                            op_id,
                            "UPSTREAM_UNAVAILABLE",
                            http_status=status,
                            request_id=request_id,
                            retryable="after_delay",
                            remediation=f"The response body was cut off ({failure}). This read has no side effects; call again.",
                            attempts=attempts,
                        )
                    )
                )
            return Answer(
                _fail(
                    diagnostic(
                        op_id,
                        "UNEXPECTED_RESPONSE",
                        http_status=status,
                        request_id=request_id,
                        retryable="never",
                        remediation=(
                            f"The server accepted {op_id} (HTTP {status}) but the response body was cut off "
                            f"({failure}). The call took effect; do not repeat it."
                        ),
                        next_action=hint,
                        attempts=attempts,
                    )
                )
            )
        data = outcome.body or b""
        if not success:
            if status == 0 or 300 <= status <= 399:
                location = headers.get("location")
                to = f" to {location[:200]}" if location else ""
                remediation = (
                    f"The server answered with a redirect{to}. Redirects are never followed on mutations; check "
                    "base_url (scheme, host, trailing slash). Whether the call took effect is unknown only if the "
                    "server applied it before redirecting."
                    if mutation
                    else f"The server answered with a redirect{to} that leaves the API's origin (or too many "
                    "redirects). It is not followed, so credentials never leave the API's host; check base_url."
                )
                return Answer(
                    _fail(
                        diagnostic(
                            op_id,
                            "UNEXPECTED_RESPONSE",
                            http_status=status or None,
                            request_id=request_id,
                            remediation=remediation,
                            next_action=hint if mutation else None,
                            attempts=attempts,
                        )
                    )
                )
            decoded = decode_body(data, headers, declared_media)
            return Answer(_fail(classify_error(call_ctx, status, headers, decoded, retry_after)))

        decoded = decode_body(data, headers, declared_media)
        configured_mode: object = self.options.validate_responses
        mode = configured_mode if configured_mode in ("off", "strict") else "warn"
        listed = field(op["agent"], "sensitive_response_fields")
        sensitive = [f for f in listed if isinstance(f, str)] if is_array(listed) else []
        after_effect = " The call took effect; do not repeat it." if mutation else ""
        problem: Diagnostic | None = None
        value: object = decoded.value
        validate = _hook(op.get("response"), "validate")
        if decoded.invalid_json:
            problem = diagnostic(
                op_id,
                "UNEXPECTED_RESPONSE",
                http_status=status,
                request_id=request_id,
                failed_parameter="response",
                expected="a JSON body",
                remediation=f"The success response announced JSON but did not parse.{after_effect}",
                attempts=attempts,
            )
        elif mode != "off" and callable(validate) and not decoded.empty:
            try:
                checked = validate(decoded.value)
            except Exception as error:
                checked = Invalid(
                    issues=[{"path": [], "message": f"the response schema failed ({describe_error(error)})"}]
                )
            if isinstance(checked, Valid):
                value = cast(Valid[object], checked).data
            else:
                issues = items_of(cast(object, checked.issues)) if isinstance(checked, Invalid) else ()
                issue = issues[0] if len(issues) > 0 else None
                path = [str(s) for s in items_of(field(issue, "path"))]
                message = str_field(issue, "message") or "a valid response"
                path_text = "".join(f"[{s}]" if _DIGITS.fullmatch(s) else f".{s}" for s in path)
                kept = (
                    " The decoded body is in the result's partial; store any value shown only once from it before "
                    "anything else."
                    if mutation and mode == "strict"
                    else ""
                )
                problem = diagnostic(
                    op_id,
                    "UNEXPECTED_RESPONSE",
                    http_status=status,
                    request_id=request_id,
                    failed_parameter=f"response{path_text}",
                    received_value=envelope_value(
                        get_path(redact_paths(decoded.value, sensitive), path),
                        any(looks_sensitive(s) for s in path),
                    ),
                    expected=message,
                    remediation=(
                        f"The success response does not match the API description at response{path_text} "
                        f"({message}).{after_effect}{kept}"
                    ),
                    attempts=attempts,
                )
        if problem is not None:
            scrubbed = scrub_diagnostic(problem, prepared.secrets)
            # The effect of a mutation happened: its body is never dropped,
            # since it can hold the only copy of a value (a one-time secret).
            if mode == "strict":
                partial = decoded.value if mutation and not decoded.invalid_json else None
                return Answer(_fail(scrubbed, partial), decoded.value)
            yield from self._emit(scrubbed)
        meta = ResponseMeta(status=status, headers=dict(headers), request_id=request_id, attempts=attempts)
        return Answer(Ok(value=None if value is UNSET else value, meta=meta), decoded.value)

    # ----------------------------------------------------------- preview

    def _preview(
        self, op: OperationDescriptor, args: object, opts: Mapping[str, object]
    ) -> Flow[Ok[PreviewResult] | Err]:
        prepared = yield from self._prepare(op, args, opts, "preview", None, None)
        if isinstance(prepared, Err):
            return prepared
        p = prepared
        agent = op["agent"]
        effects: list[str] = []
        confirmation = field(agent, "confirmation")
        message = str_field(confirmation, "message")
        if message:
            effects.append(interpolate(message, p.args, op))
        note = str_field(agent, "remediation_note")
        if note:
            effects.append(note)
        summary_fields = field(confirmation, "summary_fields")
        listed = [f for f in summary_fields if isinstance(f, str)] if is_array(summary_fields) else []
        if len(listed) > 0:
            effects.append(
                interpolate(f"Arguments: {', '.join(f'{f}={{{f}}}' for f in listed)}.", p.args, op)
            )
        summary = str_field(op, "summary")
        if summary:
            effects.append(summary)
        safety = agent["safety"]
        token = (
            None
            if safety == "read_only"
            else issue_token(self.confirmation_key, op["id"], p.args, self.now())
        )
        request: RenderedRequest = {
            "method": p.method,
            "url": p.display_url,
            "headers": p.headers.to_record(True),
            "body": p.display_body,
        }
        result: PreviewResult = {
            "operation": op["id"],
            "safety": safety,
            "request": request,
            "effects": effects,
            "confirmation_token": token,
            "expires_in_ms": None if token is None else CONFIRMATION_TTL_MS,
        }
        mode = agent["preview"]
        if mode.get("mode") == "header":
            dry = yield from self._prepare(op, args, opts, "server_preview", None, None)
            if isinstance(dry, Err):
                return dry
            answer = yield from self._send(dry, opts)
            if isinstance(answer.result, Err):
                return answer.result
            listed_fields = field(agent, "sensitive_response_fields")
            sensitive = [f for f in listed_fields if isinstance(f, str)] if is_array(listed_fields) else []
            result["server_preview"] = (
                redact_paths(json_ready(answer.raw), sensitive) if answer.raw is not UNSET else None
            )
            return Ok(value=result, meta=answer.result.meta)
        if mode.get("mode") == "endpoint":
            target_id = str_field(mode, "operation") or ""
            target = self.registry.get(target_id)
            if target is None:
                return _fail(
                    diagnostic(
                        op["id"],
                        "VALIDATION_FAILED",
                        failed_parameter="operation",
                        expected=f"the preview operation {target_id} registered with the client",
                        remediation=(
                            f"Register {target_id} (ClientCore.register or ClientOptions.operations) so preview() "
                            "can call it; nothing was sent."
                        ),
                    )
                )
            rest = {k: v for k, v in opts.items() if k not in ("confirm", "verify")}
            answer = yield from self._call(target, args, rest, None, None)
            if isinstance(answer.result, Err):
                return answer.result
            result["server_preview"] = None if answer.raw is UNSET else json_ready(answer.raw)
            return Ok(value=result, meta=answer.result.meta)
        return Ok(value=result, meta=_EMPTY_META)

    # ------------------------------------------------- pages and polling

    def _pages(
        self,
        op: OperationDescriptor,
        args: object,
        opts: Mapping[str, object],
        step: StepControl | None,
        sink: Callable[[PageAnswer], Flow[bool]],
    ) -> Flow[None]:
        pagination = field(op, "pagination")
        current: dict[str, object] | None = dict(args) if is_record(args) else None
        override: str | None = None
        previous_cursor: object = UNSET
        while True:
            answer = yield from self._call(op, current if current is not None else args, opts, override, step)
            result = answer.result
            if isinstance(result, Err):
                yield from sink(PageAnswer(result, []))
                return
            body = answer.raw
            items_field = str_field(pagination, "items_field") or ""
            found = get_path(body, items_field) if items_field else body
            items = list(items_of(found))
            next_value: object = None
            style = field(pagination, "style")
            if style == "cursor" and current is not None:
                cursor = get_path(body, str_field(pagination, "response_field") or "")
                if cursor is UNSET or cursor is None or cursor == "" or _same(cursor, previous_cursor):
                    next_value = None
                else:
                    next_value = cursor
                    previous_cursor = cursor
                    current = {**current, arg_name(op, str_field(pagination, "request_param") or ""): cursor}
            elif style == "offset" and current is not None:
                offset_key = arg_name(op, str_field(pagination, "offset_param") or "")
                limit = current.get(arg_name(op, str_field(pagination, "limit_param") or ""))
                offset = bounded(current.get(offset_key), 0, 0)
                short = is_number(limit) and len(items) < limit
                next_value = None if len(items) == 0 or short else int_like(offset + len(items))
                if next_value is not None:
                    current = {**current, offset_key: next_value}
            elif style == "page" and current is not None:
                page_key = arg_name(op, str_field(pagination, "page_param") or "")
                size = current.get(arg_name(op, str_field(pagination, "size_param") or ""))
                page = bounded(current.get(page_key), 1, 0)
                short = is_number(size) and len(items) < size
                next_value = None if len(items) == 0 or short else int_like(page + 1)
                if next_value is not None:
                    current = {**current, page_key: next_value}
            elif style == "link_header":
                link = next_link(result.meta.headers.get("link"))
                if link is not None:
                    base = override if override is not None else self._base_url() or ""
                    origin = origin_of(base)
                    try:
                        resolved = urljoin(base, link)
                    except ValueError:
                        resolved = ""
                    if origin is None or origin_of(resolved) != origin:
                        yield from sink(
                            PageAnswer(
                                Ok(value=Page(items=items, body=result.value, next=None), meta=result.meta),
                                items,
                            )
                        )
                        yield from sink(
                            PageAnswer(
                                _fail(
                                    diagnostic(
                                        op["id"],
                                        "UNEXPECTED_RESPONSE",
                                        http_status=result.meta.status,
                                        request_id=result.meta.request_id,
                                        remediation=(
                                            "The next-page Link header points to another origin; it is not followed "
                                            "so credentials never leave the API's host. Stop paginating here."
                                        ),
                                        attempts=result.meta.attempts,
                                    )
                                ),
                                [],
                            )
                        )
                        return
                    next_value = resolved
                    override = resolved
            page_result = Ok(value=Page(items=items, body=result.value, next=next_value), meta=result.meta)
            more = yield from sink(PageAnswer(page_result, items))
            if next_value is None or not more:
                return

    def _poll(
        self,
        op: OperationDescriptor,
        args: object,
        until: object,
        interval_ms: object,
        budget_ms: object,
        opts: Mapping[str, object],
        step: StepControl | None,
    ) -> Flow[Answer]:
        interval = bounded(interval_ms, _DEFAULT_POLL_INTERVAL_MS, 0)
        budget = bounded(budget_ms, 0, 0)
        started = _monotonic_ms()
        once = {**opts, "verify": False}
        while True:
            answer = yield from self._call(op, args, once, None, step)
            result = answer.result
            if isinstance(result, Err):
                return answer
            if evaluate_predicate(until, answer.raw):
                return Answer(replace(result, timed_out=False), answer.raw)
            if _monotonic_ms() - started + interval > budget:
                return Answer(replace(result, timed_out=True), answer.raw)
            yield from sleep(interval)

    def _verify(
        self, op: OperationDescriptor, args: Mapping[str, object], value: object, opts: Mapping[str, object]
    ) -> Flow[Verification]:
        hook = field(op["agent"], "verify")
        unchecked = Verification(checked=False, passed=False, observed=None)
        target_id = str_field(hook, "operation")
        target = self.registry.get(target_id) if target_id is not None else None
        if target is None:
            return unchecked
        # The hook is written against the API: argument keys and `$args`
        # references use wire names, and predicates may reference the call.
        scope: Scope = {"response": value, "args": with_wire_names(op, args)}
        hook_args = field(hook, "args")
        evaluated = evaluate_expr({} if hook_args is UNSET or hook_args is None else hook_args, scope)
        if not is_record(evaluated):
            return unchecked
        mapped = {arg_name(target, k): v for k, v in evaluated.items()}

        # A reference that does not resolve never matches (it is not dropped,
        # which would make the predicate hold vacuously).
        def refs(node: object, depth: int = 0) -> object:
            if depth > 64:
                return _UNRESOLVED
            if isinstance(node, str) and node.startswith("$"):
                found = resolve_ref(node, scope)
                return _UNRESOLVED if found is UNSET or found is None else found
            if is_array(node):
                return [refs(item, depth + 1) for item in node]
            if is_record(node):
                return {k: refs(v, depth + 1) for k, v in node.items()}
            return node

        def resolve(predicate: object) -> object:
            return refs(predicate) if is_record(predicate) and len(predicate) > 0 else None

        terminal = resolve(field(hook, "terminal"))
        expect = resolve(field(hook, "expect")) or NOTHING
        interval = bounded(field(hook, "poll_interval_ms"), _DEFAULT_POLL_INTERVAL_MS, 0)
        budget = bounded(
            field(hook, "poll_budget_ms"), _DEFAULT_VERIFY_BUDGET_MS if terminal is not None else 0, 0
        )
        rest = {k: v for k, v in opts.items() if k not in ("confirm", "idempotency_key")}
        rest["verify"] = False
        answer = yield from self._poll(
            target, mapped, terminal if terminal is not None else expect, interval, budget, rest, None
        )
        if isinstance(answer.result, Err):
            return Verification(checked=False, passed=False, observed=None, error=answer.result.error)
        listed = field(target.get("agent"), "sensitive_response_fields")
        sensitive = [f for f in listed if isinstance(f, str)] if is_array(listed) else []
        observed = None if answer.raw is UNSET else redact_paths(json_ready(answer.raw), sensitive)
        return Verification(
            checked=True,
            passed=evaluate_predicate(expect, answer.raw),
            observed=observed,
            timed_out=answer.result.timed_out is True if budget > 0 else False,
        )

    # ------------------------------------------------------------ macros

    def _macro_input(self, macro: object, input: object, name: str) -> dict[str, object] | Err:
        """The macro's input with the ``add`` defaults applied."""
        if not is_record(input):
            return _fail(
                diagnostic(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="input",
                    expected="a mapping",
                    remediation=f"Pass the input of {name} as one mapping.",
                )
            )
        effective = dict(input)
        add = field(field(macro, "input"), "add")
        for key, schema in entries_of(add).items():
            if effective.get(key, UNSET) is UNSET and is_record(schema) and "default" in schema:
                effective[key] = schema["default"]
        return effective

    def _macro_plan(
        self, macro: object, name: str
    ) -> tuple[list[tuple[Mapping[str, object], OperationDescriptor]], Safety] | Err:
        """The macro's steps resolved against the registry, and its effective
        tier: the strictest of the declared one and every step's."""

        def invalid(remediation: str) -> Err:
            return _fail(
                diagnostic(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="macro",
                    expected="a valid macro descriptor",
                    remediation=remediation,
                )
            )

        listed = field(macro, "steps")
        if not is_array(listed):
            return invalid("The macro descriptor has no steps; regenerate the SDK.")
        declared = field(macro, "safety")
        safety = str(declared) if declared in SAFETIES else "irreversible"
        steps: list[tuple[Mapping[str, object], OperationDescriptor]] = []
        for index, step in enumerate(s for s in listed if is_record(s)):
            op_id = step.get("operation")
            op = self.registry.get(op_id) if isinstance(op_id, str) else None
            if op is None:
                shown = op_id if isinstance(op_id, str) else display_json(None if op_id is UNSET else op_id)
                return invalid(
                    f"Step {index + 1} of {name} names {shown}, which is not registered with the client."
                )
            tier = safety_of(op)
            if isinstance(tier, str) and SAFETY_RANK.get(tier, 3) > SAFETY_RANK.get(safety, 3):
                safety = tier
            steps.append((step, op))
        return steps, cast(Safety, safety)

    def _step_args(
        self,
        macro: object,
        step: Mapping[str, object],
        op: OperationDescriptor,
        scope: Scope,
        pending: set[str] | None = None,
    ) -> object:
        """Step arguments evaluated against ``scope`` (a dry evaluation when
        ``pending`` names results not produced yet); for the step whose args
        the input extends, the fields the macro adds are removed."""
        expr = step.get("args")
        source: object = NOTHING if expr is UNSET or expr is None else expr
        args = evaluate_dry(source, scope, pending) if pending is not None else evaluate_expr(source, scope)
        if args is UNSET or args is None:
            args = NOTHING
        extends = str_field(field(macro, "input"), "extends")
        added = list(entries_of(field(field(macro, "input"), "add")))
        if is_record(args) and op["id"] == extends and len(added) > 0:
            args = {k: v for k, v in args.items() if k not in added}
        return args

    def _preview_macro(
        self, macro: MacroDescriptor, input: object, opts: Mapping[str, object]
    ) -> Flow[Ok[PreviewResult] | Err]:
        name = safe_name(macro, "name", "<unknown macro>")
        plan = self._macro_plan(macro, name)
        if isinstance(plan, Err):
            return plan
        effective = self._macro_input(macro, input, name)
        if isinstance(effective, Err):
            return effective
        steps, safety = plan
        if len(steps) == 0:
            return _fail(
                diagnostic(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="macro",
                    expected="a valid macro descriptor",
                    remediation=f"{name} has no steps; regenerate the SDK.",
                )
            )
        rest = {k: v for k, v in opts.items() if k not in ("confirm", "verify")}
        # A dry evaluation: the input is known, earlier steps' results are
        # placeholders (`<from step NAME: path>`).
        scope: dict[str, object] = {"input": effective}
        pending: set[str] = set()
        previews: list[MacroStepPreview] = []
        effects: list[str] = []
        summary = str_field(macro, "summary")
        if summary:
            effects.append(summary)
        key_used = False
        for index, (step, op) in enumerate(steps):
            kind = step.get("kind")
            step_kind: Literal["call", "poll", "paginate"] = (
                "poll" if kind == "poll" else "paginate" if kind == "paginate" else "call"
            )
            where = f"step {index + 1} of {name}: {op['id']}"
            args = self._step_args(macro, step, op, scope, pending)
            step_opts = {**rest, "verify": False}
            uses_key = policy_of(op) in ("caller_owned", "auto")
            if not uses_key or key_used:
                step_opts.pop("idempotency_key", None)
            elif "idempotency_key" in opts:
                key_used = True
            request: RenderedRequest | None = None
            if is_record(args):
                body = op["body"]
                encoding = body["encoding"] if body is not None else None
                # Bytes and multipart bodies cannot be encoded from a value not known yet.
                unrenderable = encoding in ("bytes", "multipart") and contains_placeholder(args)
                if not unrenderable:
                    prepared = yield from self._prepare(op, args, step_opts, "macro_preview", None, None)
                    if isinstance(prepared, Err):
                        error = prepared.error
                        return _fail(
                            with_fields(
                                error, operation=name, remediation=f"{error['remediation']} ({where})"
                            )
                        )
                    request = {
                        "method": prepared.method,
                        "url": unescape_placeholders(prepared.display_url, placeholders_in(args)),
                        "headers": prepared.headers.to_record(True),
                        "body": prepared.display_body,
                    }
            elif not contains_placeholder(args):
                return _fail(
                    diagnostic(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter="input",
                        expected="a mapping",
                        remediation=f"Step {index + 1} of {name} does not evaluate to an argument object.",
                    )
                )
            own: list[str] = []
            message = str_field(field(op["agent"], "confirmation"), "message")
            if message:
                own.append(interpolate(message, args if is_record(args) else {}, op))
            if step_kind == "poll":
                until = describe_predicate(step.get("until"))
                budget = bounded(evaluate_expr(step.get("budget_ms"), scope), _DEFAULT_MACRO_BUDGET_MS, 0)
                interval = bounded(step.get("interval_ms"), _DEFAULT_POLL_INTERVAL_MS, 0)
                own.append(
                    f"Repeats {op['id']} every {js_number(interval)} ms{f' until {until}' if until else ''}, "
                    f"for at most {js_number(budget)} ms."
                )
            elif step_kind == "paginate":
                pages = math.floor(bounded(step.get("max_pages"), _MACRO_PAGE_LIMIT, 1, 10_000))
                own.append(f"Reads up to {pages} pages of {op['id']}.")
            note = str_field(op["agent"], "remediation_note")
            if note:
                own.append(note)
            as_name = str_field(step, "as_") or None
            step_safety = op["agent"]["safety"]
            previews.append(
                {
                    "step": index + 1,
                    "kind": step_kind,
                    "operation": op["id"],
                    "as_": as_name,
                    "safety": step_safety,
                    "request": request,
                    "effects": own,
                }
            )
            effects.append(f"Step {index + 1}: {step_kind} {op['id']} ({step_safety}).")
            effects.extend(own)
            if as_name is not None:
                pending.add(as_name)
        if field(macro, "shown_once") is True:
            effects.append("The result contains values the API shows only once; store them immediately.")
        # Step 1 has no earlier results, so it is always rendered.
        first = previews[0]["request"]
        if first is None:
            return _fail(
                diagnostic(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="input",
                    expected="a mapping",
                    remediation=f"Step 1 of {name} cannot be rendered.",
                )
            )
        token = (
            None
            if safety == "read_only"
            else issue_token(self.confirmation_key, f"macro:{name}", effective, self.now())
        )
        value: PreviewResult = {
            "operation": name,
            "safety": safety,
            "request": first,
            "effects": effects,
            "confirmation_token": token,
            "expires_in_ms": None if token is None else CONFIRMATION_TTL_MS,
            "steps": previews,
        }
        return Ok(value=value, meta=_EMPTY_META)

    def _rerun_protected(self, op: OperationDescriptor, opts: Mapping[str, object]) -> bool:
        """Whether running this step again (by rerunning its macro with the
        same input and options) answers the first result instead of applying
        the effect twice."""
        policy = policy_of(op)
        if policy in ("content_identity", "content_hash", "auto"):
            return True
        return isinstance(_opt(opts, "idempotency_key"), str) and (
            policy == "caller_owned" or has_key_param(op)
        )

    def _run_macro(
        self, macro: MacroDescriptor, input: object, opts: Mapping[str, object]
    ) -> Flow[Ok[Any] | Err]:
        name = safe_name(macro, "name", "<unknown macro>")
        plan = self._macro_plan(macro, name)
        if isinstance(plan, Err):
            return plan
        effective = self._macro_input(macro, input, name)
        if isinstance(effective, Err):
            return effective
        steps, safety = plan
        unconfirmed = self._confirmed(
            name,
            f"macro:{name}",
            safety,
            effective,
            _opt(opts, "confirm"),
            "the macro's preview(...)",
            _opt(opts, "allow_confirm_true") is not False,
        )
        if unconfirmed is not None:
            return unconfirmed
        # The token is spent by the first step that sends (as an operation's
        # token is): a step failing pre-flight leaves it valid for the
        # corrected run.
        confirm = _opt(opts, "confirm")
        token = confirm if isinstance(confirm, str) else None
        run_key = _opt(opts, "idempotency_key")
        claimed = [False]

        def claim() -> Err | None:
            if claimed[0] or token is None:
                return None
            claimed[0] = True
            return self._claim_token(
                name, token, run_key if isinstance(run_key, str) else None, "the macro's preview(...)"
            )

        control = StepControl(claim)
        scope: dict[str, object] = {"input": effective}
        completed: list[str] = []
        #: Results of completed steps by `as` name: the partial of a later failure.
        produced: dict[str, object] = {}
        #: Completed mutating steps that a rerun of the macro would apply again.
        unprotected: list[str] = []
        key_used = False
        meta = _EMPTY_META
        for index, (step, op) in enumerate(steps):
            args = self._step_args(macro, step, op, scope)
            if not is_record(args):
                return _fail(
                    diagnostic(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter="input",
                        expected="a mapping",
                        remediation=f"Step {index + 1} of {name} does not evaluate to an argument object.",
                    )
                )
            uses_key = policy_of(op) in ("caller_owned", "auto")
            step_opts = {k: v for k, v in opts.items() if k != "confirm"}
            step_opts["verify"] = False
            if not uses_key or key_used:
                step_opts.pop("idempotency_key", None)
            elif "idempotency_key" in opts:
                key_used = True
            value: object = UNSET
            failure: Diagnostic | None = None
            failed_partial: object = None
            kind = step.get("kind")
            if kind == "poll":
                until = step.get("until")
                answer = yield from self._poll(
                    op,
                    args,
                    until if is_record(until) else {},
                    bounded(step.get("interval_ms"), _DEFAULT_POLL_INTERVAL_MS, 0),
                    bounded(evaluate_expr(step.get("budget_ms"), scope), _DEFAULT_MACRO_BUDGET_MS, 0),
                    step_opts,
                    control,
                )
                if isinstance(answer.result, Ok):
                    value = None if answer.result.timed_out is True else answer.raw
                    meta = answer.result.meta
                else:
                    failure = answer.result.error
            elif kind == "paginate":
                collector = _Collector(
                    math.floor(bounded(step.get("max_pages"), _MACRO_PAGE_LIMIT, 1, 10_000)), meta
                )
                yield from self._pages(op, args, step_opts, control, collector.collect)
                failure = collector.failure
                meta = collector.meta
                value = collector.items
            else:
                answer = yield from self._call(op, args, step_opts, None, control)
                if isinstance(answer.result, Ok):
                    value = answer.raw
                    meta = answer.result.meta
                else:
                    failure = answer.result.error
                    failed_partial = answer.result.partial
            as_name = str_field(step, "as_") or None
            if failure is not None:
                if failed_partial is not None and as_name is not None:
                    produced[as_name] = failed_partial
                done = (
                    f"completed before it: {', '.join(completed)}"
                    if len(completed) > 0
                    else "no step completed before it"
                )
                remediation = (
                    f"{failure['remediation']} Macro {name} stopped at step {index + 1} of {len(steps)} "
                    f"({op['id']}); {done}."
                )
                retryable = failure["retryable"]
                next_action = failure["next_action"]
                kept = list(produced)
                if len(kept) > 0:
                    once = (
                        ", including values the API shows only once: store them now"
                        if field(macro, "shown_once") is True
                        else ""
                    )
                    remediation += (
                        f" The result's partial holds the completed steps' results ({', '.join(kept)}){once}."
                    )
                if len(unprotected) > 0:
                    # Rerunning the macro would apply these steps again (a
                    # second endpoint, a lost one-time secret): finish from here.
                    remediation += (
                        f" Do not run {name} again: {', '.join(unprotected)} already took effect and has no "
                        "replay protection."
                    )
                    if retryable in ("after_delay", "same_key_only"):
                        retryable = "after_remediation"
                    finish = (
                        f"Finish {name} without rerunning it: call {op['id']} yourself with the values from the "
                        "result's partial."
                    )
                    next_action = finish if next_action is None else f"{next_action} {finish}"
                # The envelope names what was called (the macro); the
                # remediation names the step that failed.
                error = with_fields(
                    failure,
                    operation=name,
                    remediation=remediation,
                    retryable=retryable,
                    next_action=next_action,
                )
                return _fail(error, dict(produced) if len(kept) > 0 else None)
            completed.append(op["id"])
            if as_name is not None:
                scope[as_name] = value
                produced[as_name] = None if value is UNSET else json_ready(value)
            if is_mutation(op) and not self._rerun_protected(op, step_opts):
                unprotected.append(op["id"])
        output = evaluate_expr(field(macro, "output"), scope)
        return Ok(value=None if output is UNSET else json_ready(output), meta=meta)


class _Collector:
    """Gathers the items of a macro's ``paginate`` step, up to its page limit."""

    __slots__ = ("failure", "items", "limit", "meta", "pages")

    def __init__(self, limit: int, meta: ResponseMeta) -> None:
        self.limit = limit
        self.meta = meta
        self.items: list[object] = []
        self.pages = 0
        self.failure: Diagnostic | None = None

    def collect(self, page: PageAnswer) -> Flow[bool]:
        yield from ()
        if isinstance(page.result, Err):
            self.failure = page.result.error
            return False
        self.items.extend(page.items)
        self.meta = page.result.meta
        self.pages += 1
        return self.pages < self.limit


def _same(a: object, b: object) -> bool:
    try:
        return canonical_json(a) == canonical_json(b)
    except (TypeError, RecursionError):
        return False


def _valid_url(url: str) -> bool:
    try:
        parts = urlsplit(url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            return False
        if parts.username or parts.password:
            return False
        _ = parts.port
        httpx.URL(url)
    except (ValueError, httpx.InvalidURL):
        return False
    return True
