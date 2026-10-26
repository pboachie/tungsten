# SPDX-License-Identifier: Apache-2.0
"""Result rendering for MCP tool calls: diagnostic envelopes for problems the
server finds itself, JSON conversion of SDK values, redaction of sensitive
response fields and truncation of large results. Mirrors ``render.ts`` of the
TypeScript adapter (``@elysra/tungsten-mcp``)."""

from __future__ import annotations

import base64
import dataclasses
import json
from collections.abc import Callable, Mapping, Sequence
from typing import Any

#: Replacement for a sensitive response field on a repeated identical call.
REDACTED_REPEAT = "<redacted after first read>"

#: The ``shown_once`` line of the text rendering.
SHOWN_ONCE_LINE = "Store this secret now; it will not be shown again."


def clip(name: str) -> str:
    """A caller-supplied name cut for envelopes and messages."""
    return name if len(name) <= 100 else f"{name[:100]}…"


def _received_value(value: Any) -> Any:
    """Strings are cut to 200 characters in ``received_value``."""
    if isinstance(value, str):
        return value if len(value) <= 200 else f"{value[:200]}…"
    if value is None:
        return None
    try:
        text = json.dumps(value)
    except (TypeError, ValueError):
        return None
    return value if len(text) <= 200 else f"{text[:200]}…"


def envelope(
    operation: str,
    category: str,
    *,
    remediation: str,
    failed_parameter: str | None = None,
    received_value: Any = None,
    expected: str | None = None,
    retryable: str = "never",
    next_action: str | None = None,
) -> dict[str, Any]:
    """A diagnostic envelope with the documented field order."""
    return {
        "status": "error",
        "category": category,
        "operation": clip(operation),
        "http_status": None,
        "code": None,
        "failed_parameter": failed_parameter,
        "received_value": _received_value(received_value),
        "expected": expected,
        "remediation": remediation,
        "retryable": retryable,
        "retry_after_ms": None,
        "next_action": next_action,
        "request_id": None,
        "trace": {"attempts": 0},
    }


def to_json(value: Any) -> Any:
    """``value`` as plain JSON data: Pydantic models and dataclasses become
    objects (wire names), tuples lists, and bytes base64 text."""
    dump = getattr(value, "model_dump", None)
    if callable(dump):
        return to_json(dump(mode="json", by_alias=True, exclude_unset=True))
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {f.name: to_json(getattr(value, f.name)) for f in dataclasses.fields(value)}
    if isinstance(value, Mapping):
        return {str(k): to_json(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [to_json(v) for v in value]
    if isinstance(value, (bytes, bytearray, memoryview)):
        return base64.b64encode(bytes(value)).decode("ascii")
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return str(value)


def compact_json(value: Any) -> str:
    """Compact JSON for text renderings; never raises."""
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        return '"<unserializable value>"'


def canonical_json(value: Any) -> str:
    """JSON with object keys sorted at every depth."""
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def redact_paths(value: Any, paths: Sequence[str], replacement: Any) -> tuple[Any, list[str]]:
    """A copy of ``value`` with every field at the dotted ``paths`` replaced by
    ``replacement`` (arrays on the way are traversed element by element), and
    the paths that held a value."""
    found: list[str] = []
    out = value
    for path in paths:
        segments = [s for s in path.split(".") if s]
        if not segments:
            continue
        hit = False

        def replace(v: Any, i: int, segments: list[str] = segments) -> Any:
            nonlocal hit
            if isinstance(v, list):
                return [replace(x, i) for x in v]
            if not isinstance(v, dict):
                return v
            key = segments[i]
            if v.get(key) is None:
                return v
            hit = True
            return {**v, key: replacement if i == len(segments) - 1 else replace(v[key], i + 1)}

        out = replace(out, 0)
        if hit:
            found.append(path)
    return out, found


def holds_any(value: Any, paths: Sequence[str]) -> bool:
    """Whether any of the dotted ``paths`` holds a value in ``value``."""
    return len(redact_paths(value, paths, None)[1]) > 0


def _max_array_length(v: Any) -> int:
    if isinstance(v, list):
        return max([len(v), *(_max_array_length(x) for x in v)])
    if isinstance(v, dict):
        return max([0, *(_max_array_length(x) for x in v.values())])
    return 0


def _max_string_length(v: Any) -> int:
    if isinstance(v, str):
        return len(v)
    if isinstance(v, list):
        return max([0, *(_max_string_length(x) for x in v)])
    if isinstance(v, dict):
        return max([0, *(_max_string_length(x) for x in v.values())])
    return 0


def _limit_arrays(v: Any, items: int) -> Any:
    if isinstance(v, list):
        return [_limit_arrays(x, items) for x in v[:items]]
    if isinstance(v, dict):
        return {k: _limit_arrays(x, items) for k, x in v.items()}
    return v


def _limit_strings(v: Any, chars: int) -> Any:
    if isinstance(v, str):
        return f"{v[:chars]}…" if len(v) > chars else v
    if isinstance(v, list):
        return [_limit_strings(x, chars) for x in v]
    if isinstance(v, dict):
        return {k: _limit_strings(x, chars) for k, x in v.items()}
    return v


def _largest(lo: int, hi: int, fits: Callable[[int], bool]) -> int | None:
    """Largest n in [lo, hi] with fits(n), or None; fits is monotone."""
    if hi < lo or not fits(lo):
        return None
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if fits(mid):
            lo = mid
        else:
            hi = mid - 1
    return lo


def truncate(value: Any, cap: int) -> tuple[Any, str | None]:
    """``value`` cut to fit ``cap`` characters of JSON: arrays (at every depth)
    to their first items, then strings to a common length. Returns the value
    and a note for the text rendering, or ``None`` when nothing was cut."""

    def size(v: Any) -> int:
        return len(compact_json(v))

    full = size(value)
    if full <= cap:
        return value, None
    cuts: list[str] = []
    current = value
    longest = _max_array_length(current)
    if longest > 1:
        items = _largest(1, longest - 1, lambda n: size(_limit_arrays(value, n)) <= cap) or 1
        current = _limit_arrays(value, items)
        cuts.append(f"arrays are cut to their first {items} item{'' if items == 1 else 's'}")
    if size(current) > cap:
        base = current
        chars = _largest(0, max(0, _max_string_length(base) - 1), lambda n: size(_limit_strings(base, n)) <= cap) or 0
        current = _limit_strings(base, chars)
        cuts.append(f"strings to {chars} characters")
    over = " (still over the cap: the result has too many fields)" if size(current) > cap else ""
    note = (
        f"[Truncated to fit {cap} characters: the full result has {full}; {' and '.join(cuts)}{over}. "
        "Narrow the request (filters, a smaller page size) to see the rest.]"
    )
    return current, note


def skeleton(schema: Any, depth: int = 0) -> Any:
    """Placeholder value for a JSON Schema in an example call skeleton."""
    if not isinstance(schema, dict) or depth > 6:
        return "<value>"
    if "const" in schema:
        return schema["const"]
    if isinstance(schema.get("enum"), list) and schema["enum"]:
        return schema["enum"][0]
    if "default" in schema:
        return schema["default"]
    for key in ("anyOf", "oneOf"):
        options = schema.get(key)
        if isinstance(options, list):
            first = next((o for o in options if not (isinstance(o, dict) and o.get("type") == "null")), None)
            if first is not None:
                return skeleton(first, depth + 1)
    types = [t for t in schema["type"] if t != "null"] if isinstance(schema.get("type"), list) else [schema.get("type")]
    kind = types[0] if types else None
    if kind == "string":
        return f"<{schema['format']}>" if isinstance(schema.get("format"), str) else "<string>"
    if kind in ("integer", "number"):
        minimum = schema.get("minimum")
        return minimum if isinstance(minimum, (int, float)) and not isinstance(minimum, bool) else 0
    if kind == "boolean":
        return False
    if kind == "array":
        return []
    if kind == "object":
        properties = schema.get("properties") if isinstance(schema.get("properties"), dict) else {}
        required = [r for r in schema.get("required") or [] if isinstance(r, str)]
        return {name: skeleton(properties.get(name), depth + 1) for name in required}
    return "<value>"
