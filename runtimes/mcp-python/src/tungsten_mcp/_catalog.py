# SPDX-License-Identifier: Apache-2.0
"""The tool catalog read from the manifest the MCP emitter writes
(``manifest.json``, the contract in ``types.ts`` of ``@elysra/tungsten-mcp``),
and the ``tools/list`` answer of discrete mode. Mirrors ``catalog.ts``."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any

#: Value of a sensitive response field on a repeated identical call; part of
#: the advertised output schema.
from ._render import REDACTED_REPEAT

SAFETY_TIERS = ("read_only", "mutating", "destructive", "irreversible")

_VERIFICATION_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": "Result of the operation's verification hook.",
    "properties": {
        "checked": {"type": "boolean"},
        "passed": {"type": "boolean"},
        "observed": {},
        "timedOut": {"type": "boolean"},
        "error": {"type": "object"},
    },
}

#: What an error result's ``structuredContent`` (the envelope) satisfies.
_ENVELOPE_BRANCH: dict[str, Any] = {
    "properties": {"status": {"const": "error"}},
    "required": ["status", "category", "remediation"],
}

_META_READ = {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": False, "openWorldHint": False}


class ManifestError(ValueError):
    """The manifest is not one this runtime can serve."""


@dataclass(frozen=True)
class CatalogTool:
    """One tool of the manifest."""

    name: str
    kind: str
    target: str
    description: str
    input_schema: dict[str, Any]
    output_schema: dict[str, Any] | None
    annotations: dict[str, Any]
    safety: str
    idempotency: str
    idempotency_key: str | None
    confirmation_token: str | None
    sensitive_response_fields: tuple[str, ...]
    shown_once: bool


@dataclass
class Catalog:
    """The tools of a manifest, by name, in manifest order."""

    tools: list[CatalogTool]
    instructions: str
    by_name: dict[str, CatalogTool] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.by_name = {tool.name: tool for tool in self.tools}

    @property
    def previewable(self) -> list[str]:
        """The tools ``preview`` accepts: every tool that is not read-only."""
        return sorted(t.name for t in self.tools if t.safety != "read_only")


def verifies(tool: CatalogTool) -> bool:
    """Whether a call of this tool runs its verification hook (on by default
    in the MCP server for irreversible operations)."""
    return tool.kind == "operation" and tool.safety == "irreversible"


def _text(entry: Mapping[str, Any], key: str) -> str:
    value = entry.get(key)
    if not isinstance(value, str) or value == "":
        raise ManifestError(f"a manifest tool has no {key}")
    return value


def build_catalog(manifest: Mapping[str, Any]) -> Catalog:
    """Check ``manifest`` and read its tools. Raises ``ManifestError`` for a
    manifest of another version or with a malformed tool."""
    if manifest.get("manifestVersion") != 1:
        raise ManifestError(f"unsupported manifestVersion {manifest.get('manifestVersion')!r}; this runtime reads 1")
    raw_tools = manifest.get("tools")
    if not isinstance(raw_tools, list):
        raise ManifestError("the manifest has no tools list")
    tools: list[CatalogTool] = []
    for entry in raw_tools:
        if not isinstance(entry, Mapping):
            raise ManifestError("a manifest tool is not an object")
        safety = _text(entry, "safety")
        if safety not in SAFETY_TIERS:
            raise ManifestError(f"tool {entry.get('name')!r} has unknown safety {safety!r}")
        kind = _text(entry, "kind")
        if kind not in ("operation", "macro"):
            raise ManifestError(f"tool {entry.get('name')!r} has unknown kind {kind!r}")
        reserved = entry.get("reserved") if isinstance(entry.get("reserved"), Mapping) else {}
        output = entry.get("outputSchema")
        tools.append(
            CatalogTool(
                name=_text(entry, "name"),
                kind=kind,
                target=_text(entry, "target"),
                description=str(entry.get("description") or ""),
                input_schema=dict(entry.get("inputSchema") or {"type": "object"}),
                output_schema=dict(output) if isinstance(output, Mapping) else None,
                annotations=dict(entry.get("annotations") or {}),
                safety=safety,
                idempotency=str(entry.get("idempotency") or "none"),
                idempotency_key=reserved.get("idempotencyKey") or None,
                confirmation_token=reserved.get("confirmationToken") or None,
                sensitive_response_fields=tuple(entry.get("sensitiveResponseFields") or ()),
                shown_once=bool(entry.get("shownOnce")),
            )
        )
    names = [t.name for t in tools]
    if len(set(names)) != len(names) or "preview" in names:
        raise ManifestError("manifest tool names must be unique and must not be `preview`")
    by_mode = manifest.get("instructionsByMode")
    instructions = by_mode.get("discrete") if isinstance(by_mode, Mapping) else None
    if not isinstance(instructions, str):
        instructions = str(manifest.get("instructions") or "")
    return Catalog(tools=tools, instructions=instructions)


def _allow_redaction(schema: dict[str, Any], segments: list[str]) -> dict[str, Any]:
    """``schema`` with the field at a dotted path also allowed to be the redaction marker."""
    if not segments:
        return {"anyOf": [schema, {"const": REDACTED_REPEAT}]}
    if schema.get("type") == "array" and isinstance(schema.get("items"), dict):
        return {**schema, "items": _allow_redaction(schema["items"], segments)}
    properties = schema.get("properties")
    key = segments[0]
    if not isinstance(properties, dict) or not isinstance(properties.get(key), dict):
        return schema
    return {**schema, "properties": {**properties, key: _allow_redaction(properties[key], segments[1:])}}


def advertised_output_schema(tool: CatalogTool) -> dict[str, Any] | None:
    """The ``outputSchema`` advertised for a tool: the manifest's schema when it
    describes an object, widened for what the server adds (the verification
    result, the redaction marker, the error envelope). Otherwise none (the
    body is then returned as ``{value}``)."""
    schema = tool.output_schema
    if not schema or schema.get("type") != "object":
        return None
    body = {k: v for k, v in schema.items() if k not in ("$schema", "$defs")}
    if verifies(tool):
        properties = body.get("properties") if isinstance(body.get("properties"), dict) else {}
        if "verification" not in properties:
            body = {**body, "properties": {**properties, "verification": _VERIFICATION_SCHEMA}}
    for path in tool.sensitive_response_fields:
        body = _allow_redaction(body, [s for s in path.split(".") if s])
    out: dict[str, Any] = {}
    if "$schema" in schema:
        out["$schema"] = schema["$schema"]
    out["type"] = "object"
    if "$defs" in schema:
        out["$defs"] = schema["$defs"]
    out["anyOf"] = [body, _ENVELOPE_BRANCH]
    return out


def list_tools(catalog: Catalog) -> list[dict[str, Any]]:
    """The ``tools/list`` answer: identical for every session and deterministic."""
    listed: list[dict[str, Any]] = []
    for tool in sorted(catalog.tools, key=lambda t: t.name):
        entry: dict[str, Any] = {
            "name": tool.name,
            "description": tool.description,
            "inputSchema": tool.input_schema,
            "annotations": dict(tool.annotations),
        }
        output = advertised_output_schema(tool)
        if output is not None:
            entry["outputSchema"] = output
        listed.append(entry)
    previewable = catalog.previewable
    if previewable:
        listed.append(
            {
                "name": "preview",
                "description": (
                    "Preview a non-read-only tool call without executing it: the rendered request, its effects and "
                    "a confirmation_token bound to these exact arguments (required by destructive and irreversible "
                    "tools)."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string", "enum": previewable},
                        "arguments": {
                            "type": "object",
                            "description": (
                                "Arguments of the call to preview: the tool's input object, including its reserved "
                                "fields (idempotency key, confirmation token) when it has them."
                            ),
                        },
                    },
                    "required": ["tool"],
                    "additionalProperties": False,
                },
                "annotations": dict(_META_READ),
            }
        )
    return listed
