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

import anyio
from tungsten_runtime import AsyncClientCore, Ok, StreamEvent

from ._catalog import PROGRESSIVE_TOOLS, TOOL_META_KEY, Catalog, CatalogTool, verifies
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
    skeleton,
    to_json,
    truncate,
)
from ._search import bm25, suggest

DEFAULT_SEARCH_LIMIT = 10
#: Events a streamed call collects before it stops reading (the result then
#: has ``truncated: true``).
MAX_STREAM_EVENTS = 1000
SEARCH_HINT = (
    "Call describe_tool(name) for the schema, preview(name, arguments) before destructive or irreversible calls, "
    "invoke(name, arguments) to execute (invoke_read for read_only tools)."
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
        catalog = self.catalog
        if catalog.mode == "discrete":
            if name == "preview" and catalog.previewable:
                bad = self._check_args(name, args, {"tool": ("string", True), "arguments": ("object", False)})
                if bad is not None:
                    return bad
                found = self._lookup(args["tool"], "tool", name)
                return found if isinstance(found, dict) else await self._preview(found, args.get("arguments"))
            tool = catalog.by_name.get(name)
            if tool is None:
                return self._unknown_tool(name)
            prepared = self._prepare(tool, args)
            return self._render(tool, await self._execute(prepared), prepared)
        if name == "invoke_read" and not catalog.has_read_only:
            return self._unknown_tool(name)
        if name == "search_tools":
            spec = {"query": ("string", True), "cluster": ("string", False), "limit": ("integer", False)}
            return self._check_args(name, args, spec) or self._search(args)
        if name == "describe_tool":
            bad = self._check_args(name, args, {"name": ("string", True)})
            if bad is not None:
                return bad
            found = self._lookup(args["name"], "name", name)
            return found if isinstance(found, dict) else self._describe(found)
        if name in ("invoke_read", "invoke", "preview"):
            bad = self._check_args(name, args, {"name": ("string", True), "arguments": ("object", False)})
            if bad is not None:
                return bad
            found = self._lookup(args["name"], "name", name)
            if isinstance(found, dict):
                return found
            if name == "invoke_read" and found.safety != "read_only":
                return self._with_tool_meta(
                    self._failure(
                        None,
                        envelope(
                            name,
                            "VALIDATION_FAILED",
                            failed_parameter="name",
                            received_value=found.name,
                            expected="the name of a read_only tool",
                            remediation=(
                                f"{found.name} is {found.safety}, so invoke_read refuses it; nothing ran. Call "
                                "preview(name, arguments) and then invoke(name, arguments) instead."
                            ),
                        ),
                    ),
                    found,
                )
            if name == "preview":
                return self._with_tool_meta(await self._preview(found, args.get("arguments")), found)
            prepared = self._prepare(found, args.get("arguments"))
            return self._with_tool_meta(self._render(found, await self._execute(prepared), prepared), found)
        if name == "list_clusters":
            return self._check_args(name, args, {}) or self._list_clusters()
        return self._unknown_tool(name)

    # ---------------------------------------------------------- arguments

    def _check_args(self, name: str, args: Mapping[str, Any], spec: Mapping[str, tuple[str, bool]]) -> Result | None:
        """A meta tool's own arguments against ``spec``: name -> (type, required),
        the type being ``string`` (non-empty), ``integer`` or ``object`` (an
        object or its JSON text)."""
        allowed = list(spec)
        for key, value in args.items():
            if key not in spec and value is not None:
                close = suggest(key, allowed, (), 1)
                did = f" (did you mean {close[0]}?)" if close else ""
                return self._failure(
                    None,
                    envelope(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter=key,
                        received_value=value,
                        expected=f"one of: {', '.join(allowed)}" if allowed else "no arguments",
                        remediation=f"Remove {clip(key)}; {name} does not accept it{did}.",
                    ),
                )
        for key, (kind, required) in spec.items():
            value = args.get(key)
            if value is None:
                if not required:
                    continue
                return self._failure(
                    None,
                    envelope(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter=key,
                        expected=f"a {kind}",
                        remediation=f"Pass {key}; {name} cannot be called without it.",
                    ),
                )
            if kind == "string":
                ok = isinstance(value, str) and value != ""
            elif kind == "integer":
                ok = isinstance(value, int) and not isinstance(value, bool)
            else:
                ok = isinstance(value, (Mapping, str))
            if not ok:
                article = "an" if kind == "integer" else "a"
                expected = (
                    "an object (or its JSON text)"
                    if kind == "object"
                    else f"{article} {'non-empty string' if kind == 'string' else kind}"
                )
                shown = "an object" if kind == "object" else f"{article} {kind}"
                return self._failure(
                    None,
                    envelope(
                        name,
                        "VALIDATION_FAILED",
                        failed_parameter=key,
                        received_value=value,
                        expected=expected,
                        remediation=f"Pass {key} as {shown} and call {name} again.",
                    ),
                )
        return None

    def _ranked(self, text: str) -> list[str]:
        """Tool names sharing words with ``text``, best first."""
        docs = self.catalog.documents
        return [docs[i].name for i, _ in bm25(self.catalog.index, len(docs), text)]

    def _lookup(self, name: str, parameter: str, via: str) -> CatalogTool | Result:
        """The catalog tool called ``name``, or an error result with suggestions."""
        tool = self.catalog.by_name.get(name)
        if tool is not None:
            return tool
        close = suggest(name, [t.name for t in self.catalog.tools], self._ranked(name))
        where = "tools/list" if self.catalog.mode == "discrete" else "search_tools(query)"
        did = f" Did you mean: {', '.join(close)}?" if close else ""
        return self._failure(
            None,
            envelope(
                via,
                "VALIDATION_FAILED",
                failed_parameter=parameter,
                received_value=name,
                expected="the name of a tool of this server",
                remediation=f"No tool is named {clip(name)}.{did} Use {where} to find tools.",
            ),
        )

    def _unknown_tool(self, name: str) -> Result:
        catalog = self.catalog
        if catalog.mode == "progressive" and name in catalog.by_name:
            return self._failure(
                None,
                envelope(
                    name,
                    "VALIDATION_FAILED",
                    failed_parameter="name",
                    received_value=name,
                    expected="a tool from tools/list",
                    remediation=(
                        f'{name} is reached through invoke: call invoke({{"name": "{name}", "arguments": {{...}}}}) '
                        "(describe_tool gives its schema)."
                    ),
                ),
            )
        if catalog.mode == "discrete":
            listed = [t.name for t in catalog.tools] + (["preview"] if catalog.previewable else [])
            ranked = self._ranked(name)
        else:
            listed = [t for t in PROGRESSIVE_TOOLS if t != "invoke_read" or catalog.has_read_only]
            ranked = []
        close = suggest(name, listed, ranked)
        did = f" Did you mean: {', '.join(close)}?" if close else ""
        return self._failure(
            None,
            envelope(
                name,
                "VALIDATION_FAILED",
                failed_parameter="name",
                received_value=name,
                expected="a tool from tools/list",
                remediation=f"No tool is named {clip(name)}.{did} Call tools/list for the available tools.",
            ),
        )

    def _preview_form(self, tool: CatalogTool) -> str:
        key = "tool" if self.catalog.mode == "discrete" else "name"
        return f'preview({{"{key}": "{tool.name}", "arguments": ...}})'

    def _call_form(self, tool: CatalogTool) -> str:
        if self.catalog.mode == "discrete":
            return tool.name
        return f'invoke({{"name": "{tool.name}", "arguments": ...}})'

    def _with_tool_meta(self, result: Result, tool: CatalogTool) -> Result:
        """``result`` with the tool's tier and hints in ``_meta``, so a host that
        sees only ``invoke`` or ``preview`` still learns what it just ran."""
        meta = dict(result.get("_meta") or {})
        meta[TOOL_META_KEY] = {"name": tool.name, "safety": tool.safety, "annotations": tool.hints}
        return {**result, "_meta": meta}

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
                return await self._collect_stream(descriptor, args, opts, prepared.tool)
            if verifies(prepared.tool):
                opts["verify"] = True
            result = await core.call(descriptor, args, opts)  # type: ignore[arg-type]
        if isinstance(result, Ok):
            return _Outcome(ok=True, value=result.value, verification=result.verification)
        partial = getattr(result, "partial", None)
        return _Outcome(ok=False, error=dict(result.error), partial=partial, has_partial=partial is not None)

    async def _collect_stream(
        self, op: Mapping[str, Any], args: Mapping[str, Any], opts: dict[str, Any], tool: CatalogTool
    ) -> _Outcome:
        """Collect the events of a stream in order: the events, or the failure
        that ended the stream with the events before it as ``partial``. Stops
        at ``MAX_STREAM_EVENTS`` events (``truncated: true``) and fails when the
        events exceed ``max_stream_bytes`` of JSON or the collection runs past
        ``max_stream_ms``."""
        core = self._core
        assert core is not None
        events: list[Any] = []
        max_bytes = self.options.max_stream_bytes
        max_ms = self.options.max_stream_ms

        def exceeded(what: str) -> _Outcome:
            count = f"{len(events)} {'event' if len(events) == 1 else 'events'}"
            effect = (
                " The events collected so far are in partial."
                if tool.safety == "read_only"
                else " The call took effect; do not repeat it. The events collected so far are in partial."
            )
            return _Outcome(
                ok=False,
                error=envelope(
                    op["id"],
                    "UNEXPECTED_RESPONSE",
                    failed_parameter="events",
                    expected=what,
                    remediation=(
                        f"The stream was abandoned after {count} ({what}; ServerOptions.max_stream_bytes and "
                        f"max_stream_ms).{effect}"
                    ),
                ),
                partial={"events": events},
                has_partial=True,
            )

        stream = core.stream(op, args, opts)  # type: ignore[arg-type]
        size = 0
        try:
            with anyio.move_on_after(max_ms / 1000) as scope:
                async for item in stream:
                    if not isinstance(item, StreamEvent):
                        partial = {"events": events}
                        return _Outcome(ok=False, error=dict(item.error), partial=partial, has_partial=True)
                    value = to_json(item.value)
                    size += len(compact_json(value).encode())
                    if size > max_bytes:
                        return exceeded(f"at most {max_bytes} bytes of events")
                    events.append(value)
                    if len(events) >= MAX_STREAM_EVENTS:
                        return _Outcome(ok=True, value={"events": events, "truncated": True})
            if scope.cancelled_caught:
                return exceeded(f"at most {max_ms} ms to collect the events")
            return _Outcome(ok=True, value={"events": events})
        finally:
            stream.cancel()
            with anyio.CancelScope(shield=True):
                await stream.aclose()

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
                f"Call {self._preview_form(tool)} with the same arguments, then call {self._call_form(tool)} "
                f'again with "{token_field}" set to its confirmation_token (only a token from this session is '
                "accepted, never true)."
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
                f'To execute, call {self._call_form(tool)} with the same arguments plus "{field_name}": "{token}" (valid '
                f"{valid}, for these exact arguments, in this session only)."
            )
        elif confirmed:
            lines.append(
                "This tool needs confirmation but takes no confirmation token, so it cannot be executed through this server."
            )
        elif preview.get("safety") == "read_only":
            lines.append(f"Read-only: call {self._call_form(tool)} directly.")
        else:
            lines.append(f"No confirmation is needed: call {self._call_form(tool)} with the same arguments to execute.")
        return self._success(preview, lines, "steps" if isinstance(preview.get("steps"), list) else None)

    # ------------------------------------------------------ progressive mode

    def _search(self, args: Mapping[str, Any]) -> Result:
        catalog = self.catalog
        query = args["query"]
        limit = args.get("limit")
        limit = DEFAULT_SEARCH_LIMIT if limit is None else limit
        if limit < 1 or limit > 50:
            return self._failure(
                None,
                envelope(
                    "search_tools",
                    "VALIDATION_FAILED",
                    failed_parameter="limit",
                    received_value=limit,
                    expected="an integer from 1 to 50",
                    remediation="Pass limit between 1 and 50.",
                ),
            )
        cluster = None
        if isinstance(args.get("cluster"), str):
            cluster = next((c for c in catalog.clusters if c.name == args["cluster"]), None)
            if cluster is None:
                names = [c.name for c in catalog.clusters]
                close = suggest(args["cluster"], names)
                did = f" Did you mean: {', '.join(close)}?" if close else ""
                return self._failure(
                    None,
                    envelope(
                        "search_tools",
                        "VALIDATION_FAILED",
                        failed_parameter="cluster",
                        received_value=args["cluster"],
                        expected=f"one of: {', '.join(names)}" if names else "no cluster (this API has none)",
                        remediation=(
                            f"No cluster is named {clip(args['cluster'])}.{did} Call list_clusters() for the "
                            "clusters, or search without one."
                        ),
                    ),
                )
        docs = catalog.documents
        hits = [
            docs[i]
            for i, _ in bm25(catalog.index, len(docs), query)
            if cluster is None or catalog.in_cluster(docs[i], cluster)
        ]
        hint = SEARCH_HINT
        if not hits and cluster is not None:
            hits = [t for t in catalog.tools if catalog.in_cluster(t, cluster)]
            hint = f"No tool matched the query; these are the tools of cluster {cluster.name}. {SEARCH_HINT}"
        elif not hits:
            hint = (
                "No tool matched. Use other words from the task (resource names, verbs), or call list_clusters() "
                "and search one cluster."
            )
        results = [
            {
                "name": t.name,
                "summary": t.summary,
                "safety": t.safety,
                "annotations": t.hints,
                "idempotency": t.idempotency,
                "schema_tokens": t.schema_tokens,
                "call_with": t.call_with,
            }
            for t in hits[:limit]
        ]
        return self._success({"results": results, "hint": hint}, [])

    def _describe(self, tool: CatalogTool) -> Result:
        example = skeleton(tool.input_schema)
        call: dict[str, Any] = example if isinstance(example, dict) else {}
        key_field = tool.idempotency_key
        token_field = tool.confirmation_token
        if key_field:
            properties = tool.input_schema.get("properties")
            key_schema = properties.get(key_field) if isinstance(properties, dict) else None
            label = "UUIDv4" if isinstance(key_schema, dict) and key_schema.get("format") == "uuid" else "unique key"
            call[key_field] = f"<{label}: generate once, persist with the intent, reuse on every retry>"
        if token_field:
            if tool.safety in ("destructive", "irreversible"):
                call[token_field] = "<confirmation_token from preview with these arguments>"
            else:
                call.pop(token_field, None)
        op = self._core.operation(tool.target) if tool.kind == "operation" and self._core is not None else None
        remediation = op["agent"].get("remediation") if op and isinstance(op.get("agent"), Mapping) else None
        if tool.safety == "read_only":
            steps = "Read-only: invoke it directly."
        elif tool.safety == "mutating":
            keyed = f': pass "{key_field}" and reuse it on every retry of the same intent' if key_field else ""
            steps = f"Mutating{keyed}; preview(name, arguments) shows the request first."
        else:
            tier = "Irreversible" if tool.safety == "irreversible" else "Destructive"
            set_to = f'"{token_field}" set to' if token_field else ""
            steps = (
                f"{tier}: call preview(name, arguments) first, then invoke with the same arguments and {set_to} its "
                "confirmation_token."
            )
        return self._success(
            {
                "name": tool.name,
                "kind": tool.kind,
                "summary": tool.summary,
                "description": tool.description,
                "safety": tool.safety,
                "idempotency": tool.idempotency,
                "cluster": tool.cluster,
                "annotations": tool.annotations,
                "input_schema": tool.input_schema,
                "output_schema": tool.output_schema,
                "reserved_fields": {"idempotency_key": key_field, "confirmation_token": token_field},
                "sensitive_response_fields": list(tool.sensitive_response_fields),
                "shown_once": tool.shown_once,
                "remediation": to_json(remediation) if isinstance(remediation, Mapping) else {},
                "schema_tokens": tool.schema_tokens,
                "call_with": tool.call_with,
                "example": {"tool": tool.call_with, "arguments": {"name": tool.name, "arguments": call}},
                "hint": steps,
            },
            [],
        )

    def _list_clusters(self) -> Result:
        catalog = self.catalog
        clusters = [
            {"name": c.name, "summary": c.summary, "tools": sum(1 for t in catalog.tools if catalog.in_cluster(t, c))}
            for c in catalog.clusters
        ]
        unclustered = sum(1 for t in catalog.tools if not any(catalog.in_cluster(t, c) for c in catalog.clusters))
        return self._success(
            {
                "clusters": clusters,
                "unclustered": unclustered,
                "hint": "search_tools(query, cluster) searches one cluster.",
            },
            [],
        )


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
