# SPDX-License-Identifier: Apache-2.0
"""One MCP session: its own ``AsyncClientCore`` (so confirmation tokens and
shown-once state never cross sessions) and the discrete tool handlers. Mirrors
``session.ts`` of the TypeScript adapter for the subset this runtime serves.

Every outcome is a tool result, never a protocol error: failures are
``isError: true`` with the diagnostic envelope verbatim as
``structuredContent`` and its remediation in the text, so the model sees what
to do next. Results are plain MCP JSON (``content``, ``structuredContent``,
``isError``, ``_meta``)."""

from __future__ import annotations

import base64
import binascii
import contextlib
import hashlib
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any

from tungsten_runtime import AsyncClientCore, Ok

from ._catalog import Catalog, CatalogTool, verifies
from ._options import ServerOptions
from ._render import (
    REDACTED_REPEAT,
    SHOWN_ONCE_LINE,
    canonical_json,
    clip,
    compact_json,
    envelope,
    holds_any,
    redact_paths,
    to_json,
    truncate,
)

SAFETY_TIERS = ("read_only", "mutating", "destructive", "irreversible")
_BASE64 = re.compile(r"^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$")
_BASE64URL = re.compile(r"^[A-Za-z0-9_-]*$")

Result = dict[str, Any]


@dataclass
class _Prepared:
    """A tool call with its SDK arguments and options separated."""

    tool: CatalogTool
    #: The SDK args object, or the unparsable input (passed through so the
    #: runtime answers its own VALIDATION_FAILED envelope).
    args: Any
    idempotency_key: Any = None
    confirm: str | None = None
    #: An argument this layer could not decode (nothing is sent).
    invalid: dict[str, Any] | None = None


@dataclass
class _Outcome:
    ok: bool
    value: Any = None
    verification: Any = None
    error: dict[str, Any] | None = None
    partial: Any = None
    has_partial: bool = False


def _decode_base64(text: str) -> bytes | None:
    """The bytes of base64 (standard, padded) or base64url (unpadded) text, or
    None when ``text`` is neither."""
    try:
        if _BASE64.match(text):
            return base64.b64decode(text, validate=True)
        if _BASE64URL.match(text) and len(text) % 4 != 1:
            return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
    except (binascii.Error, ValueError):
        return None
    return None


def _streams(op: Mapping[str, Any], args: Mapping[str, Any]) -> bool:
    """Whether a call of ``op`` reads its event stream."""
    spec = op.get("stream")
    if not isinstance(spec, Mapping):
        return False
    first = next((r.get("media_type") or "" for r in op.get("responses", []) if r.get("kind") == "success"), "")
    if first.split(";")[0].strip().lower() == "text/event-stream":
        return True
    flag = spec.get("flag")
    body = op.get("body")
    if not flag or not isinstance(body, Mapping):
        return False
    shape = body.get("shape") or {}
    if shape.get("kind") == "merged":
        return args.get(flag) is True
    inner = args.get(shape.get("arg", ""))
    return isinstance(inner, Mapping) and inner.get(flag) is True


@dataclass
class Session:
    """The tool handlers of one MCP session."""

    catalog: Catalog
    options: ServerOptions
    _core: AsyncClientCore | None = None
    _core_error: str | None = None
    #: Digests of identical replayable calls whose sensitive or shown-once
    #: results this session has already returned. Digests only: no argument
    #: or secret is kept.
    _shown: set[str] = field(default_factory=set)

    def __post_init__(self) -> None:
        try:
            core = self.options.create_core()
        except Exception as error:  # reported per call, never raised
            self._core_error = f"create_core failed: {error}"[:300]
            return
        if isinstance(core, AsyncClientCore):
            self._core = core
        else:
            self._core_error = "create_core did not return an AsyncClientCore"

    async def aclose(self) -> None:
        """Release the client's connection pool."""
        if self._core is not None:
            await self._core.aclose()

    async def call(self, name: Any, arguments: Any) -> Result:
        """Handle one ``tools/call`` with its raw ``name`` and ``arguments``;
        never raises. Arguments may be an object or its JSON text."""
        if not isinstance(name, str) or name == "":
            return self._failure(
                None,
                envelope(
                    "tools/call",
                    "VALIDATION_FAILED",
                    failed_parameter="name",
                    received_value=name,
                    expected="a tool name from tools/list",
                    remediation="Pass the tool's name as a string, from tools/list.",
                ),
            )
        parsed = {} if arguments is None else arguments
        if isinstance(parsed, str):
            with contextlib.suppress(ValueError):
                parsed = json.loads(parsed)
        if not isinstance(parsed, Mapping):
            return self._failure(
                None,
                envelope(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="arguments",
                    received_value=arguments,
                    expected="a JSON object (or its JSON text)",
                    remediation=f"Pass arguments as a JSON object of {clip(name)}'s input schema and call it again.",
                ),
            )
        try:
            return await self._dispatch(name, dict(parsed))
        except Exception as error:
            return self._failure(
                None,
                envelope(
                    name,
                    "UNEXPECTED_RESPONSE",
                    remediation=(
                        f"The MCP server failed while handling {clip(name)} ({error}). This is a bug in "
                        "tungsten-mcp; report it. If the call may have been sent, check its effect before "
                        "repeating it."
                    ),
                ),
            )

    async def _dispatch(self, name: str, args: dict[str, Any]) -> Result:
        if name == "preview" and self.catalog.previewable:
            bad = self._check_args(name, args)
            if bad is not None:
                return bad
            tool = self.catalog.by_name.get(args["tool"])
            if tool is None:
                return self._unknown(args["tool"], "tool", name)
            return await self._preview(tool, args.get("arguments"))
        tool = self.catalog.by_name.get(name)
        if tool is None:
            return self._unknown(name, "name", name)
        prepared = self._prepare(tool, args)
        return self._render(tool, await self._execute(prepared), prepared)

    # ---------------------------------------------------------- arguments

    def _check_args(self, name: str, args: Mapping[str, Any]) -> Result | None:
        """The ``preview`` meta tool's own arguments: ``tool`` (required) and
        ``arguments`` (an object or its JSON text)."""
        for key, value in args.items():
            if key not in ("tool", "arguments") and value is not None:
                return self._failure(
                    None,
                    envelope(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter=key,
                        received_value=value,
                        expected="one of: tool, arguments",
                        remediation=f"Remove {clip(key)}; {name} does not accept it.",
                    ),
                )
        tool = args.get("tool")
        if not isinstance(tool, str) or tool == "":
            return self._failure(
                None,
                envelope(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="tool",
                    received_value=tool,
                    expected="a non-empty string",
                    remediation=f"Pass tool, the name of the tool to preview; {name} cannot be called without it.",
                ),
            )
        inner = args.get("arguments")
        if inner is not None and not isinstance(inner, (Mapping, str)):
            return self._failure(
                None,
                envelope(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="arguments",
                    received_value=inner,
                    expected="an object (or its JSON text)",
                    remediation=f"Pass arguments as an object and call {name} again.",
                ),
            )
        return None

    def _unknown(self, name: str, parameter: str, via: str) -> Result:
        listed = sorted(self.catalog.by_name)
        if self.catalog.previewable:
            listed.append("preview")
        words = set(re.split(r"[^a-z0-9]+", name.lower())) - {""}
        close = [n for n in listed if words & set(n.split("_"))][:3]
        hint = f" Did you mean: {', '.join(close)}?" if close else ""
        return self._failure(
            None,
            envelope(
                via,
                "VALIDATION_FAILED",
                failed_parameter=parameter,
                received_value=name,
                expected="a tool from tools/list",
                remediation=f"No tool is named {clip(name)}.{hint} Call tools/list for the available tools.",
            ),
        )

    def _prepare(self, tool: CatalogTool, raw: Any) -> _Prepared:
        args: Any = {} if raw is None else raw
        if isinstance(args, str):
            # Not JSON: the runtime reports the non-object arguments.
            with contextlib.suppress(ValueError):
                args = json.loads(args)
        if not isinstance(args, Mapping):
            return _Prepared(tool, args)
        key_field = tool.idempotency_key
        token_field = tool.confirmation_token
        sdk_args = {k: v for k, v in args.items() if k not in (key_field, token_field)}
        token = args.get(token_field) if token_field else None
        prepared = _Prepared(
            tool,
            sdk_args,
            idempotency_key=args.get(key_field) if key_field else None,
            # Only a string token is forwarded: True never confirms through MCP.
            confirm=token if isinstance(token, str) and token != "" else None,
        )
        # A binary body travels as base64 text in JSON (its input schema says
        # `contentEncoding: base64`); the SDK takes the bytes. Decoded here, so
        # preview and call digest the same value.
        op = self._core.operation(tool.target) if tool.kind == "operation" and self._core is not None else None
        body = op.get("body") if op else None
        if body and body.get("encoding") == "bytes" and body.get("shape", {}).get("kind") == "arg":
            name = body["shape"]["arg"]
            if isinstance(sdk_args.get(name), str):
                data = _decode_base64(sdk_args[name])
                if data is not None:
                    sdk_args[name] = data
                else:
                    prepared.invalid = envelope(
                        tool.target,
                        "VALIDATION_FAILED",
                        failed_parameter=name,
                        received_value=clip(sdk_args[name]),
                        expected="the raw bytes as base64 text",
                        remediation=(
                            f"Pass {name} as the base64 encoding of the raw bytes (standard with padding, or "
                            "base64url); this server decodes it and sends the bytes. Nothing was sent."
                        ),
                    )
        return prepared

    def _call_options(self, prepared: _Prepared, with_confirm: bool) -> dict[str, Any]:
        # Only preview tokens are forwarded, so the runtime must not offer `True`.
        opts: dict[str, Any] = {
            "allow_confirm_true": False,
            "max_response_bytes": self.options.max_response_bytes,
        }
        # A non-string key is passed through so the runtime reports it.
        if prepared.idempotency_key is not None:
            opts["idempotency_key"] = prepared.idempotency_key
        if with_confirm and prepared.confirm is not None:
            opts["confirm"] = prepared.confirm
        if self.options.tool_deadline_ms:
            opts["deadline_ms"] = self.options.tool_deadline_ms
        return opts

    # ------------------------------------------------------------ calling

    def _unavailable(self, what: str, tool: CatalogTool) -> _Outcome:
        return _Outcome(
            ok=False,
            error=envelope(
                tool.target,
                "UNEXPECTED_RESPONSE",
                remediation=f"The MCP server is misconfigured: {what}. Nothing was sent; report it and do not retry.",
            ),
        )

    def _resolve(self, tool: CatalogTool) -> tuple[str, Any] | _Outcome:
        """The operation or macro behind ``tool``, checked against the
        manifest's safety tier, or why it cannot be called."""
        core = self._core
        if core is None:
            return self._unavailable(self._core_error or "no API client", tool)
        if tool.kind == "macro":
            macro = core.macro(tool.target)
            if macro is None:
                return self._unavailable(
                    f"macro {tool.target} of tool {tool.name} is not in the Python SDK (regenerate it, or "
                    "the SDK cannot express this macro)",
                    tool,
                )
            tier = SAFETY_TIERS.index(macro["safety"]) if macro.get("safety") in SAFETY_TIERS else -1
            for step in macro.get("steps", []):
                op = core.operation(step.get("operation", ""))
                safety = op["agent"]["safety"] if op else None
                if safety not in SAFETY_TIERS:
                    return self._unavailable(
                        f"macro {tool.target} has an unregistered step or invalid step safety", tool
                    )
                tier = max(tier, SAFETY_TIERS.index(safety))
            if tier < 0 or SAFETY_TIERS[tier] != tool.safety:
                return self._unavailable(
                    f"manifest safety for {tool.name} disagrees with its macro and steps; regenerate the manifest "
                    "and SDK together",
                    tool,
                )
            return ("macro", macro)
        op = core.operation(tool.target)
        if op is None:
            return self._unavailable(
                f"operation {tool.target} of tool {tool.name} is not registered with the client core", tool
            )
        if op["agent"]["safety"] != tool.safety:
            return self._unavailable(
                f"manifest safety for {tool.name} disagrees with its operation; regenerate the manifest and SDK together",
                tool,
            )
        return ("operation", op)

    async def _execute(self, prepared: _Prepared) -> _Outcome:
        if prepared.invalid is not None:
            return _Outcome(ok=False, error=prepared.invalid)
        target = self._resolve(prepared.tool)
        if isinstance(target, _Outcome):
            return target
        core = self._core
        assert core is not None
        opts = self._call_options(prepared, True)
        kind, descriptor = target
        args = prepared.args
        if kind == "macro":
            result = await core.run_macro(descriptor, args, opts)  # type: ignore[arg-type]
        else:
            if isinstance(args, Mapping) and _streams(descriptor, args):
                return _Outcome(
                    ok=False,
                    error=envelope(
                        prepared.tool.target,
                        "VALIDATION_FAILED",
                        remediation=(
                            f"{prepared.tool.name} answers an event stream, which this experimental Python MCP "
                            "server does not collect yet. Nothing was sent; use the TypeScript MCP server (`mcp` "
                            "target) or the SDK's stream() for this operation."
                        ),
                    ),
                )
            if verifies(prepared.tool):
                opts["verify"] = True
            result = await core.call(descriptor, args, opts)  # type: ignore[arg-type]
        if isinstance(result, Ok):
            return _Outcome(ok=True, value=result.value, verification=result.verification)
        partial = getattr(result, "partial", None)
        return _Outcome(ok=False, error=dict(result.error), partial=partial, has_partial=partial is not None)

    def _identity(self, prepared: _Prepared) -> str:
        """Digest identifying an identical call: tool, SDK arguments and key."""
        try:
            args = canonical_json(to_json(prepared.args))
        except (TypeError, ValueError):
            args = "<unserializable>"
        key = prepared.idempotency_key if isinstance(prepared.idempotency_key, str) else ""
        return hashlib.sha256(f"{prepared.tool.name}\n{args}\n{key}".encode()).hexdigest()

    def _replays(self, prepared: _Prepared) -> bool:
        """Whether an identical repeat of this call is a replay of the first
        (the same answer, no new effect)."""
        tool = prepared.tool
        if tool.safety == "read_only":
            return True
        if tool.idempotency in ("content_identity", "content_hash"):
            return True
        if tool.idempotency in ("caller_owned", "auto"):
            return isinstance(prepared.idempotency_key, str) and prepared.idempotency_key != ""
        return False

    # ------------------------------------------------------------ results

    def _render(self, tool: CatalogTool, outcome: _Outcome, prepared: _Prepared) -> Result:
        if not outcome.ok:
            assert outcome.error is not None
            return self._failure(tool, outcome.error, outcome.partial, outcome.has_partial)
        body = to_json(outcome.value)
        lines: list[str] = []
        sensitive = tool.sensitive_response_fields
        if sensitive or tool.shown_once:
            # Only a replay returns what an earlier identical call returned; a
            # repeat that is a new effect returns a new secret, which must be shown.
            identity = self._identity(prepared) if self._replays(prepared) else None
            if identity is not None and identity in self._shown:
                body, found = redact_paths(body, sensitive, REDACTED_REPEAT)
                if found:
                    were = "was" if len(found) == 1 else "were"
                    are = "is" if len(found) == 1 else "are"
                    lines.append(
                        f"{', '.join(found)} {were} returned by the first identical call in this session and {are} redacted here."
                    )
            elif not sensitive or holds_any(body, sensitive):
                if identity is not None:
                    self._shown.add(identity)
                if tool.shown_once:
                    lines.append(SHOWN_ONCE_LINE)
        structured: dict[str, Any] | None = (
            dict(body) if isinstance(body, dict) else None if body is None else {"value": body}
        )
        if outcome.verification is not None:
            verification = to_json(outcome.verification)
            structured = structured if structured is not None else {}
            if "verification" not in structured:
                structured["verification"] = verification
            else:
                lines.append(f"verification: {compact_json(verification)}")
        return self._success(structured, lines)

    def _success(self, structured: dict[str, Any] | None, lines: list[str], rendered: str | None = None) -> Result:
        value, note = truncate(structured, self.options.max_result_chars)
        shown = value
        if rendered is not None and isinstance(value, dict) and rendered in value:
            shown = {k: v for k, v in value.items() if k != rendered}
        first = "OK (no content)." if structured is None else compact_json(shown)
        text = "\n".join([first, *lines, *([note] if note else [])])
        result: Result = {"content": [{"type": "text", "text": text}]}
        if structured is not None:
            result["structuredContent"] = value
        return result

    def _failure(
        self, tool: CatalogTool | None, error: dict[str, Any], partial: Any = None, has_partial: bool = False
    ) -> Result:
        """An error result: the envelope verbatim and its remediation as text."""
        error = to_json(error)
        lines = [f"{error.get('category')} (retryable: {error.get('retryable')}): {error.get('remediation')}"]
        if error.get("next_action"):
            lines.append(f"next_action: {error['next_action']}")
        detail = [
            f"failed_parameter: {error['failed_parameter']}" if error.get("failed_parameter") else None,
            f"expected: {error['expected']}" if error.get("expected") else None,
            f"retry_after_ms: {error['retry_after_ms']}" if error.get("retry_after_ms") is not None else None,
            f"code: {error['code']}" if isinstance(error.get("code"), str) and error["code"] else None,
        ]
        if any(detail):
            lines.append("; ".join(d for d in detail if d))
        if tool is not None:
            lines.extend(self._hints(tool, error))
        if has_partial:
            value, note = truncate(to_json(partial), self.options.max_result_chars)
            lines.append(
                "Already produced before the failure (store any one-time secret in it now; it is not repeated): "
                + compact_json(value)
            )
            if note:
                lines.append(note)
        return {"content": [{"type": "text", "text": "\n".join(lines)}], "structuredContent": error, "isError": True}

    def _hints(self, tool: CatalogTool, error: Mapping[str, Any]) -> list[str]:
        """Name this server's fields and tools where the runtime names SDK ones."""
        hints: list[str] = []
        if error.get("code") == "DEADLINE_EXCEEDED":
            deadline = self.options.tool_deadline_ms
            hints.append(
                f"Here deadline_ms is the server's per-tool-call deadline ({deadline} ms, "
                "ServerOptions.tool_deadline_ms); a later call gets a fresh one."
                if deadline
                else "Here deadline_ms is the server's per-tool-call deadline (ServerOptions.tool_deadline_ms)."
            )
        key_field = tool.idempotency_key
        if error.get("failed_parameter") == "idempotency_key" and key_field:
            hints.append(f'In this tool the idempotency key is the argument "{key_field}".')
        if error.get("category") == "CONFIRMATION_REQUIRED":
            token_field = tool.confirmation_token
            hints.append(
                f'Call preview({{"tool": "{tool.name}", "arguments": ...}}) with the same arguments, then call '
                f'{tool.name} again with "{token_field}" set to its confirmation_token (only a token from this '
                "session is accepted, never true)."
                if token_field
                else "This tool takes no confirmation token, so it cannot be confirmed through this server."
            )
        return hints

    async def _preview(self, tool: CatalogTool, raw: Any) -> Result:
        prepared = self._prepare(tool, raw)
        if prepared.invalid is not None:
            return self._failure(tool, prepared.invalid)
        target = self._resolve(tool)
        if isinstance(target, _Outcome):
            return self._render(tool, target, prepared)
        core = self._core
        assert core is not None
        opts = self._call_options(prepared, False)
        kind, descriptor = target
        if kind == "macro":
            result = await core.preview_macro(descriptor, prepared.args, opts)  # type: ignore[arg-type]
        else:
            result = await core.preview(descriptor, prepared.args, opts)  # type: ignore[arg-type]
        if not isinstance(result, Ok):
            partial = getattr(result, "partial", None)
            return self._failure(tool, dict(result.error), partial, partial is not None)
        preview = to_json(result.value)
        lines = _step_lines(preview)
        field_name = tool.confirmation_token
        confirmed = any(t in ("destructive", "irreversible") for t in (tool.safety, preview.get("safety")))
        token = preview.get("confirmation_token")
        if confirmed and field_name and token is not None:
            expires = preview.get("expires_in_ms")
            valid = "briefly" if expires is None else f"{round(expires / 1000)} s"
            lines.append(
                f'To execute, call {tool.name} with the same arguments plus "{field_name}": "{token}" (valid '
                f"{valid}, for these exact arguments, in this session only)."
            )
        elif confirmed:
            lines.append(
                "This tool needs confirmation but takes no confirmation token, so it cannot be executed through this server."
            )
        elif preview.get("safety") == "read_only":
            lines.append(f"Read-only: call {tool.name} directly.")
        else:
            lines.append(f"No confirmation is needed: call {tool.name} with the same arguments to execute.")
        return self._success(preview, lines, "steps" if isinstance(preview.get("steps"), list) else None)


def _step_lines(preview: Mapping[str, Any]) -> list[str]:
    """A macro preview's steps as text: each step's request and effects."""
    steps = preview.get("steps")
    if not isinstance(steps, list) or not steps:
        return []
    lines = ["Steps (nothing is sent by this preview; <from step NAME: path> is a value an earlier step returns):"]
    for step in steps:
        bound = f", result as {step.get('as_')}" if step.get("as_") else ""
        request = step.get("request")
        if request is None:
            shown = "its request is built from an earlier step's whole result, so it cannot be shown before it runs"
        else:
            body = request.get("body")
            shown = f"{request.get('method')} {request.get('url')}" + (
                "" if body is None else f" with body {compact_json(body)}"
            )
        lines.append(
            f"{step.get('step')}. {step.get('kind')} {step.get('operation')} ({step.get('safety')}{bound}): {shown}"
        )
        lines.extend(f"   - {effect}" for effect in step.get("effects") or [])
    return lines
