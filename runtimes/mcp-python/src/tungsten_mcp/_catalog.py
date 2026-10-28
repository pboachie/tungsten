# SPDX-License-Identifier: Apache-2.0
"""The tool catalog read from the manifest the MCP emitter writes
(``manifest.json``, the contract in ``types.ts`` of ``@elysra/tungsten-mcp``),
and the ``tools/list`` answer of each disclosure mode. Mirrors ``catalog.ts``."""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any

from ._render import REDACTED_REPEAT
from ._search import SearchIndex

SAFETY_TIERS = ("read_only", "mutating", "destructive", "irreversible")
#: The disclosure modes.
MODES = ("discrete", "progressive")
#: The meta tools of progressive mode (`run_script` is never served here).
PROGRESSIVE_TOOLS = ("search_tools", "describe_tool", "invoke", "invoke_read", "preview", "list_clusters")
#: MCP tool names: 1-128 of `[A-Za-z0-9_.-]`.
_TOOL_NAME = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
_TIER_SUFFIX = re.compile(r"\s*\[[^\]]*\]\s*$")

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

#: The ``_meta`` key of invoke and preview results naming the tool they ran.
TOOL_META_KEY = "tungsten/tool"

_META_READ = {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True, "openWorldHint": False}
_CLOSED = {"additionalProperties": False}


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
    cluster: str | None = None
    schema_tokens: float = 0
    #: The description without its trailing ``[...]`` tier and key rule.
    summary: str = ""

    @property
    def hints(self) -> dict[str, bool]:
        """The hints a host gates by: the annotations without the title."""
        return {k: bool(self.annotations.get(k)) for k in ("readOnlyHint", "destructiveHint", "idempotentHint")}

    @property
    def call_with(self) -> str:
        """The progressive meta tool that calls this tool."""
        return "invoke_read" if self.safety == "read_only" else "invoke"


@dataclass(frozen=True)
class Cluster:
    name: str
    summary: str | None
    tools: tuple[str, ...]


@dataclass
class Catalog:
    """The tools of a manifest in the mode served."""

    mode: str
    tools: list[CatalogTool]
    #: One entry per manifest tool, in manifest order (the search index's
    #: document numbers).
    documents: list[CatalogTool]
    clusters: list[Cluster]
    index: SearchIndex
    instructions: str
    by_name: dict[str, CatalogTool] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.by_name = {tool.name: tool for tool in self.tools}

    @property
    def previewable(self) -> list[str]:
        """The tools discrete ``preview`` accepts: every tool that is not read-only."""
        return sorted(t.name for t in self.tools if t.safety != "read_only")

    @property
    def has_read_only(self) -> bool:
        return any(t.safety == "read_only" for t in self.tools)

    def in_cluster(self, tool: CatalogTool, cluster: Cluster) -> bool:
        """Whether ``tool`` belongs to ``cluster`` (its own field or the cluster's list)."""
        return tool.cluster == cluster.name or tool.name in cluster.tools


def verifies(tool: CatalogTool) -> bool:
    """Whether a call of this tool runs its verification hook (on by default
    in the MCP server for irreversible operations)."""
    return tool.kind == "operation" and tool.safety == "irreversible"


def _text_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value != "" else None


def _annotations(safety: str, idempotency: str, raw: Any) -> dict[str, Any]:
    """The tool's MCP annotations: a manifest may make the hints more cautious
    than its safety tier, never less cautious."""
    derived = {
        "readOnlyHint": safety == "read_only",
        "destructiveHint": safety in ("destructive", "irreversible"),
        "idempotentHint": safety == "read_only",
        "openWorldHint": False,
    }
    given = raw if isinstance(raw, Mapping) else {}

    def flag(key: str) -> bool:
        value = given.get(key)
        return value if isinstance(value, bool) else derived[key]

    # Keyed policies retry as no-ops with the same key, so they are idempotent.
    takes_key = idempotency in ("caller_owned", "auto", "content_hash")
    out: dict[str, Any] = {
        "readOnlyHint": derived["readOnlyHint"] and flag("readOnlyHint"),
        "destructiveHint": derived["destructiveHint"] or flag("destructiveHint"),
        "idempotentHint": (derived["idempotentHint"] or takes_key) and flag("idempotentHint"),
        "openWorldHint": flag("openWorldHint"),
    }
    if isinstance(given.get("title"), str) and given["title"]:
        out = {"title": given["title"], **out}
    return out


def _read_tool(entry: Any, position: int) -> CatalogTool:
    if not isinstance(entry, Mapping):
        raise ManifestError(f"manifest tool {position} is not an object")
    name = entry.get("name")
    if not isinstance(name, str) or not _TOOL_NAME.match(name):
        raise ManifestError(f"manifest tool {position}: name is not a valid MCP tool name")
    kind = entry.get("kind")
    if kind not in ("operation", "macro"):
        raise ManifestError(f"tool {name}: kind is neither operation nor macro")
    target = _text_or_none(entry.get("target"))
    if target is None:
        raise ManifestError(f"tool {name}: no target")
    schema = entry.get("inputSchema")
    if not isinstance(schema, Mapping) or schema.get("type") != "object":
        raise ManifestError(f"tool {name}: inputSchema is not an object schema")
    safety = entry.get("safety")
    if safety not in SAFETY_TIERS:
        raise ManifestError(f"tool {name}: unknown safety tier {safety!r}")
    idempotency = entry.get("idempotency") if isinstance(entry.get("idempotency"), str) else "none"
    reserved = entry.get("reserved") if isinstance(entry.get("reserved"), Mapping) else {}
    output = entry.get("outputSchema")
    description = entry.get("description") if isinstance(entry.get("description"), str) else ""
    tokens = entry.get("schemaTokens")
    sensitive = entry.get("sensitiveResponseFields")
    return CatalogTool(
        name=name,
        kind=kind,
        target=target,
        description=description,
        input_schema=dict(schema),
        output_schema=dict(output) if isinstance(output, Mapping) else None,
        annotations=_annotations(safety, idempotency, entry.get("annotations")),
        safety=safety,
        idempotency=idempotency,
        idempotency_key=_text_or_none(reserved.get("idempotencyKey")),
        confirmation_token=_text_or_none(reserved.get("confirmationToken")),
        sensitive_response_fields=tuple(f for f in sensitive if isinstance(f, str) and f)
        if isinstance(sensitive, list)
        else (),
        shown_once=entry.get("shownOnce") is True,
        cluster=_text_or_none(entry.get("cluster")),
        schema_tokens=tokens
        if isinstance(tokens, (int, float)) and not isinstance(tokens, bool) and tokens >= 0
        else 0,
        summary=_TIER_SUFFIX.sub("", description).strip(),
    )


def build_catalog(manifest: Mapping[str, Any], mode: str | None = None) -> Catalog:
    """Check ``manifest`` and read its tools, in ``mode`` (default: the
    manifest's). Raises ``ManifestError`` for a manifest of another version, a
    malformed tool or a duplicate or reserved name."""
    if manifest.get("manifestVersion") != 1:
        raise ManifestError(f"unsupported manifestVersion {manifest.get('manifestVersion')!r}; this runtime reads 1")
    raw_tools = manifest.get("tools")
    if not isinstance(raw_tools, list):
        raise ManifestError("the manifest has no tools list")
    requested = mode or manifest.get("mode")
    if requested not in MODES:
        threshold = manifest.get("threshold") if isinstance(manifest.get("threshold"), int) else 24
        requested = "progressive" if len(raw_tools) > threshold else "discrete"
    reserved_names = {"preview", "run_script"} if requested == "discrete" else set()
    tools: list[CatalogTool] = []
    for position, entry in enumerate(raw_tools):
        tool = _read_tool(entry, position)
        if tool.name in reserved_names:
            raise ManifestError(f"tool {tool.name}: the name is a {requested}-mode meta tool")
        if any(t.name == tool.name for t in tools):
            raise ManifestError(f"duplicate tool name {tool.name}")
        tools.append(tool)
    raw_clusters = manifest.get("clusters") if isinstance(manifest.get("clusters"), list) else []
    clusters = sorted(
        (
            Cluster(
                name=c["name"],
                summary=c.get("summary") if isinstance(c.get("summary"), str) else None,
                tools=tuple(t for t in c.get("tools") or () if isinstance(t, str)),
            )
            for c in raw_clusters
            if isinstance(c, Mapping) and isinstance(c.get("name"), str) and c["name"]
        ),
        key=lambda c: c.name,
    )
    by_mode = manifest.get("instructionsByMode")
    instructions = by_mode.get(requested) if isinstance(by_mode, Mapping) else None
    if not isinstance(instructions, str):
        instructions = manifest.get("instructions") if isinstance(manifest.get("instructions"), str) else ""
    return Catalog(
        mode=requested,
        tools=tools,
        documents=list(tools),
        clusters=clusters,
        index=SearchIndex.read(manifest.get("index")),
        instructions=instructions,
    )


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


def _arguments_schema(what: str) -> dict[str, Any]:
    return {
        "type": "object",
        "description": (
            f"{what}: the tool's input object, including its reserved fields (idempotency key, confirmation token) "
            "when it has them."
        ),
    }


def list_tools(catalog: Catalog) -> list[dict[str, Any]]:
    """The ``tools/list`` answer: identical for every session and deterministic."""
    if catalog.mode == "discrete":
        return _discrete_tools(catalog)
    cluster: dict[str, Any] = {
        "type": "string",
        "description": "Restrict the search to one cluster (see list_clusters).",
    }
    if catalog.clusters:
        cluster["enum"] = [c.name for c in catalog.clusters]
    named = {"name": {"type": "string", "minLength": 1}}
    listed: list[dict[str, Any]] = [
        {
            "name": "search_tools",
            "description": (
                f"Search the {len(catalog.tools)} API tools by keywords. Returns name, summary, safety tier, MCP "
                "annotations, idempotency policy, schema token cost and the tool to call it with."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "description": 'Words describing the task, e.g. "replay a failed webhook".',
                    },
                    "cluster": cluster,
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10},
                },
                "required": ["query"],
                **_CLOSED,
            },
            "annotations": dict(_META_READ),
        },
        {
            "name": "describe_tool",
            "description": (
                "Full input and output schema of one tool, its safety tier and annotations, reserved fields, "
                "remediation table and an example call."
            ),
            "inputSchema": {"type": "object", "properties": named, "required": ["name"], **_CLOSED},
            "annotations": dict(_META_READ),
        },
        {
            "name": "invoke",
            "description": (
                "Execute any tool. Returns the response body, or the error envelope with remediation. Destructive "
                "and irreversible tools need the confirmation_token from preview with the same arguments. "
                "Read-only tools: use invoke_read."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {**named, "arguments": _arguments_schema("Arguments of the tool")},
                "required": ["name"],
                **_CLOSED,
            },
            "annotations": {
                "readOnlyHint": False,
                "destructiveHint": True,
                "idempotentHint": False,
                "openWorldHint": False,
            },
        },
    ]
    if catalog.has_read_only:
        listed.append(
            {
                "name": "invoke_read",
                "description": (
                    "Execute one read-only tool (safety tier read_only); any other tier is refused, use invoke. "
                    "Safe to allow without confirmation."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {**named, "arguments": _arguments_schema("Arguments of the tool")},
                    "required": ["name"],
                    **_CLOSED,
                },
                "annotations": dict(_META_READ),
            }
        )
    listed.extend(
        [
            {
                "name": "preview",
                "description": (
                    "Preview a tool call without executing it: the rendered request, its effects and a "
                    "confirmation_token bound to these exact arguments."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {**named, "arguments": _arguments_schema("Arguments of the call to preview")},
                    "required": ["name"],
                    **_CLOSED,
                },
                "annotations": {**_META_READ, "idempotentHint": False},
            },
            {
                "name": "list_clusters",
                "description": "The tool clusters with their summaries and tool counts.",
                "inputSchema": {"type": "object", "properties": {}, **_CLOSED},
                "annotations": dict(_META_READ),
            },
        ]
    )
    return listed


def _discrete_tools(catalog: Catalog) -> list[dict[str, Any]]:
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
                        "arguments": _arguments_schema("Arguments of the call to preview"),
                    },
                    "required": ["tool"],
                    **_CLOSED,
                },
                "annotations": {**_META_READ, "idempotentHint": False},
            }
        )
    return listed
