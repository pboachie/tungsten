# SPDX-License-Identifier: Apache-2.0
"""Pure helpers of the core over descriptors and arguments: descriptor
checks, argument paths, sensitive-field handling, wire-name resolution, text
for remediation and previews, URLs and ``Link`` / ``Retry-After`` headers
(the module-level functions of ``runtimes/ts/src/client.ts`` and
``transport.ts``)."""

from __future__ import annotations

import datetime as dt
import math
import re
from collections.abc import Mapping, Sequence
from email.utils import parsedate_to_datetime
from urllib.parse import unquote_plus, urlsplit

from ._json import canonical_json, display_json
from ._serialize import EncodedBody, display_form, enc, form_enc
from ._util import (
    REDACTED,
    envelope_value,
    field,
    get_path,
    is_array,
    is_record,
    looks_sensitive,
    redact_paths,
    split_path,
    str_field,
    utf8,
)
from .sentinels import UNSET
from .types import OperationDescriptor, ParamDescriptor

METHODS = frozenset({"GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE"})
SAFETIES = frozenset({"read_only", "mutating", "destructive", "irreversible"})
POLICIES = frozenset({"none", "auto", "caller_owned", "content_hash", "content_identity"})
ENCODINGS = frozenset({"json", "form", "multipart", "bytes", "text"})
LOCATIONS = frozenset({"path", "query", "header", "cookie"})
SAFETY_RANK: Mapping[str, int] = {"read_only": 0, "mutating": 1, "destructive": 2, "irreversible": 3}

_DIGITS = re.compile(r"[0-9]+")


def safe_name(value: object, key: str, fallback: str) -> str:
    """``value[key]`` when it is a non-empty string, else the fallback."""
    found = str_field(value, key)
    return found if found else fallback


def safe_id(op: object) -> str:
    return safe_name(op, "id", "<unknown operation>")


def descriptor_problem(op: object) -> str | None:
    """Structural problems that would make the descriptor unusable, or None."""
    if not is_record(op):
        return "the operation descriptor is not a mapping"
    op_id = op.get("id")
    if not isinstance(op_id, str) or op_id == "":
        return "`id` must be a non-empty string"
    if op.get("method") not in METHODS:
        return "`method` must be an HTTP method"
    if not isinstance(op.get("path"), str):
        return "`path` must be a string"
    params = op.get("params")
    if not is_array(params):
        return "`params` must be a list"
    for p in params:
        if (
            not is_record(p)
            or not isinstance(p.get("name"), str)
            or not isinstance(p.get("wire"), str)
            or p.get("location") not in LOCATIONS
        ):
            return "every parameter needs string `name` and `wire` and a valid `location`"
    body = op.get("body")
    if body is not None:
        if not is_record(body) or not is_record(body.get("shape")):
            return "`body.shape` must be a mapping"
        if body.get("encoding") not in ENCODINGS or not isinstance(body.get("media_type"), str):
            return "`body` needs a valid `encoding` and a string `media_type`"
        shape = body.get("shape")
        kind = field(shape, "kind")
        fields = field(shape, "fields")
        if kind == "merged":
            if not is_array(fields) or not all(
                is_record(f) and isinstance(f.get("arg"), str) and isinstance(f.get("wire"), str)
                for f in fields
            ):
                return "`body.shape` must be {kind: merged, fields: [{arg, wire}]} or {kind: arg, arg}"
        elif not (kind == "arg" and isinstance(field(shape, "arg"), str)):
            return "`body.shape` must be {kind: merged, fields: [{arg, wire}]} or {kind: arg, arg}"
    stream = op.get("stream")
    if stream is not None:
        if not is_record(stream):
            return "`stream` must be a mapping"
        if "done" in stream and not isinstance(stream["done"], str):
            return "`stream.done` must be a string"
        if "flag" in stream and not isinstance(stream["flag"], str):
            return "`stream.flag` must be a string"
    if not is_array(op.get("responses")):
        return "`responses` must be a list"
    if not is_array(op.get("security")):
        return "`security` must be a list of lists"
    rpc = op.get("rpc")
    if rpc is not None and (
        not is_record(rpc)
        or not isinstance(rpc.get("field"), str)
        or not isinstance(rpc.get("params_field"), str)
    ):
        return "`rpc` needs `field` and `params_field`"
    status = op.get("status")
    if not is_record(status) or status.get("kind") not in ("implemented", "gated"):
        return "`status.kind` must be implemented or gated"
    agent = op.get("agent")
    if not is_record(agent):
        return "`agent` must be a mapping"
    if agent.get("safety") not in SAFETIES:
        return "`agent.safety` must be a safety tier"
    if field(field(agent, "idempotency"), "policy") not in POLICIES:
        return "`agent.idempotency.policy` must be an idempotency policy"
    if not is_record(agent.get("preview")):
        return "`agent.preview` must be a mapping"
    return None


def arg_params(op: OperationDescriptor) -> list[ParamDescriptor]:
    """Parameters supplied by the caller (not auth, key or origin)."""
    return [p for p in op["params"] if p.get("role") not in ("idempotency_key", "origin", "auth")]


def merged_fields(op: OperationDescriptor) -> list[tuple[str, str]]:
    """``(arg, wire)`` of a merged body's fields."""
    body = op["body"]
    if body is None:
        return []
    shape = body["shape"]
    return [(f["arg"], f["wire"]) for f in shape["fields"]] if shape["kind"] == "merged" else []


def body_arg(op: OperationDescriptor) -> str | None:
    body = op["body"]
    if body is None:
        return None
    shape = body["shape"]
    return shape["arg"] if shape["kind"] == "arg" else None


def with_stream_flag(op: OperationDescriptor, spec: object, args: object) -> object:
    """The args of a stream call: the stream's request flag set to ``True``,
    in the body field of a merged body or inside an object body argument."""
    flag = str_field(spec, "flag")
    if flag is None or not is_record(args):
        return args
    arg = next((name for name, wire in merged_fields(op) if wire == flag), None)
    if arg is not None:
        return {**args, arg: True}
    whole = body_arg(op)
    inner = args.get(whole) if whole is not None else None
    if whole is not None and is_record(inner):
        return {**args, whole: {**inner, flag: True}}
    return args


def _format_path(segments: Sequence[str | int]) -> str:
    return "".join(f"[{s}]" if isinstance(s, int) or _DIGITS.fullmatch(s) else f".{s}" for s in segments)


def argument_path(op: OperationDescriptor, path: Sequence[str | int]) -> str:
    """JSON path of an argument issue: ``body.<wire>`` for a merged body
    field (its wire name, as every runtime reports it: the path is in the
    request body), ``body.x`` inside a whole-body argument, else ``args.x``
    (the argument name of this SDK)."""
    if len(path) > 0:
        head = str(path[0])
        wire = next((w for a, w in merged_fields(op) if a == head), None)
        if wire is not None:
            return f"body{_format_path([wire, *path[1:]])}"
        if body_arg(op) == head:
            return f"body{_format_path(path[1:])}"
    return "args" if len(path) == 0 else f"args{_format_path(path)}"


def sensitive_request_paths(op: OperationDescriptor) -> list[str]:
    listed = field(field(op, "agent"), "sensitive_request_fields")
    return [p for p in listed if isinstance(p, str) and p != ""] if is_array(listed) else []


def dotted_arg_path(path: Sequence[str | int]) -> str:
    """An args path without array indices, dotted (``users.0.pin`` -> ``users.pin``)."""
    return ".".join(s for s in path if isinstance(s, str) and _DIGITS.fullmatch(s) is None)


def sensitive_arg(op: OperationDescriptor, path: Sequence[str | int]) -> bool:
    head = path[0] if len(path) > 0 else None
    if any(p.get("sensitive") is True and p["name"] == head for p in op["params"]):
        return True
    dotted = dotted_arg_path(path)
    if dotted != "" and any(dotted == f or dotted.startswith(f"{f}.") for f in sensitive_request_paths(op)):
        return True
    return any(isinstance(s, str) and looks_sensitive(s) for s in path)


def redact_below(op: OperationDescriptor, path: Sequence[str | int], value: object) -> object:
    """``value`` (the argument at ``path``) with the sensitive fields below it redacted."""
    dotted = dotted_arg_path(path)
    prefix = "" if dotted == "" else f"{dotted}."
    below = [f[len(prefix) :] for f in sensitive_request_paths(op) if f.startswith(prefix)]
    return redact_paths(value, below) if len(below) > 0 else value


def sensitive_body_paths(op: OperationDescriptor) -> list[str]:
    """Sensitive argument paths relative to the request body as sent (the rpc
    envelope's params member for rpc operations); ``""`` is the whole body.
    A merged body's members are its fields' wire names."""
    paths = sensitive_request_paths(op)
    rpc = op["rpc"]
    wires = dict(merged_fields(op))
    arg = body_arg(op)
    relative: list[str]
    if len(wires) > 0:
        relative = []
        for p in paths:
            segments = split_path(p)
            if len(segments) > 0 and segments[0] in wires:
                relative.append(".".join([wires[segments[0]], *segments[1:]]))
    elif arg is not None:
        relative = [
            "" if p == arg else p[len(arg) + 1 :] for p in paths if p == arg or p.startswith(f"{arg}.")
        ]
    else:
        relative = paths if rpc is not None and op["body"] is None else []
    if rpc is None:
        return relative
    params_field = rpc["params_field"]
    return [params_field if p == "" else f"{params_field}.{p}" for p in relative]


def values_at(value: object, segments: Sequence[str], out: list[str], depth: int = 0) -> None:
    """Every string or number found at a dotted path, through arrays."""
    if depth > 64:
        return
    if is_array(value):
        for item in value:
            values_at(item, segments, out, depth + 1)
        return
    if len(segments) == 0:
        if isinstance(value, str):
            out.append(value)
        elif isinstance(value, int | float) and not isinstance(value, bool):
            out.append(display_json(value))
        elif is_record(value):
            for item in value.values():
                values_at(item, [], out, depth + 1)
        return
    if is_record(value) and segments[0] in value:
        values_at(value[segments[0]], segments[1:], out, depth + 1)


def with_wire_names(op: OperationDescriptor, args: Mapping[str, object]) -> dict[str, object]:
    """``args`` plus each parameter's and merged body field's value under its
    wire name, so references written against the API (agent.yml
    ``{device_id}``, ``$args.endpoint_id``) find arguments whose Python name
    differs."""
    out = dict(args)
    pairs = [(p["name"], p["wire"]) for p in op["params"]] + merged_fields(op)
    for name, wire in pairs:
        if name in args and args[name] is not UNSET and wire not in out:
            out[wire] = args[name]
    return out


def arg_path(op: OperationDescriptor, path: list[str]) -> list[str]:
    """A path whose first segment is a wire name, rewritten to the argument name."""
    if len(path) == 0:
        return path
    head = path[0]
    for name, wire in [(p["name"], p["wire"]) for p in op["params"]] + merged_fields(op):
        if wire == head and name != head:
            return [name, *path[1:]]
    return path


def arg_name(op: object, key: str) -> str:
    """The argument name of a parameter given by argument or wire name."""
    params = field(op, "params")
    listed = [p for p in params if is_record(p)] if is_array(params) else []
    for p in listed:
        if p.get("name") == key:
            return key
    for p in listed:
        if p.get("wire") == key and isinstance(p.get("name"), str):
            return str(p.get("name"))
    return key


_PLACEHOLDER_FIELD = re.compile(r"\{([^{}]+)\}")


def interpolate(template: str, args: Mapping[str, object], op: OperationDescriptor) -> str:
    def replace(match: re.Match[str]) -> str:
        path = arg_path(op, match.group(1).strip().split("."))
        value = get_path(args, path)
        if value is UNSET or value is None:
            return "<unset>"
        if sensitive_arg(op, path):
            return REDACTED
        shown = envelope_value(value, False)
        return shown if isinstance(shown, str) else display_json(shown)

    return _PLACEHOLDER_FIELD.sub(replace, template)


def observable_body(encoding: str, encoded: EncodedBody) -> bytes | None:
    """What middleware may see of an encoded body: the body itself, unless
    the preview rendering redacted something in it; then that rendering,
    serialized like the body."""
    if encoded.body is None:
        return None
    try:
        changed = canonical_json(encoded.display) != canonical_json(encoded.raw)
    except (TypeError, RecursionError):
        changed = True
    if not changed:
        return encoded.body
    if encoding == "form" and is_record(encoded.display):
        return display_form(encoded.display)
    display = encoded.display
    return utf8(display if isinstance(display, str) else display_json(display))


def unescape_placeholders(url: str, placeholders: Sequence[str]) -> str:
    """A preview's display URL with placeholders shown as written instead of
    percent-encoded. Only renderings change; nothing with placeholders is
    ever sent."""
    out = url
    for text in dict.fromkeys(placeholders):
        out = out.replace(enc(text), text)
    return out


def short_json(value: object) -> str:
    """JSON for remediation text, cut to 80 characters."""
    text = display_json(value)
    return f"{text[:77]}..." if len(text) > 80 else text


def with_arguments(args: object) -> str:
    """`` with name "value", ...`` naming a call's arguments; "" when there are none."""
    if not is_record(args):
        return ""
    shown = [f"{k} {short_json(v)}" for k, v in args.items()]
    return f" with {', '.join(shown)}" if len(shown) > 0 else ""


def sent_arguments(op: OperationDescriptor, args: Mapping[str, object]) -> str:
    """`` (name "value", ...)``: the scalar, non-sensitive arguments a call was
    made with (at most four, by wire name), to recognize what it created."""
    wires = {p["name"]: p["wire"] for p in op["params"]} | dict(merged_fields(op))
    shown: list[str] = []
    for key, value in args.items():
        if len(shown) == 4:
            break
        if not isinstance(value, str | int | float | bool) or sensitive_arg(op, [key]):
            continue
        shown.append(f"{wires.get(key, key)} {short_json(value)}")
    return f" ({', '.join(shown)})" if len(shown) > 0 else ""


def int_like(value: float) -> int | float:
    return int(value) if math.isfinite(value) and value.is_integer() else value


# ------------------------------------------------------------------- URLs


def origin_of(url: str) -> str | None:
    """``scheme://host:port`` of an absolute http(s) URL, or None."""
    try:
        parts = urlsplit(url)
        scheme = parts.scheme.lower()
        host = parts.hostname
        port = parts.port
    except ValueError:
        return None
    if scheme not in ("http", "https") or not host:
        return None
    return f"{scheme}://{host}:{port if port is not None else 443 if scheme == 'https' else 80}"


def has_credentials(url: str) -> bool:
    try:
        parts = urlsplit(url)
        return bool(parts.username) or bool(parts.password)
    except ValueError:
        return True


def _query_pairs(query: str) -> list[tuple[str, str]]:
    pairs: list[tuple[str, str]] = []
    for chunk in query.split("&"):
        if chunk == "":
            continue
        name, _, value = chunk.partition("=")
        pairs.append((unquote_plus(name), unquote_plus(value)))
    return pairs


def _split_query(url: str) -> tuple[str, str, str]:
    rest, hash_mark, fragment = url.partition("#")
    base, _, query = rest.partition("?")
    return base, query, f"{hash_mark}{fragment}"


def query_names(url: str) -> set[str]:
    return {name for name, _ in _query_pairs(_split_query(url)[1])}


def with_search_params(url: str, updates: Sequence[tuple[str, str]], only_missing: bool) -> str:
    """``URLSearchParams.set`` for each update (or only for names not present),
    the query serialized as ``URLSearchParams`` does."""
    base, query, fragment = _split_query(url)
    pairs = _query_pairs(query)
    for name, value in updates:
        names = [n for n, _ in pairs]
        if name in names:
            if only_missing:
                continue
            first = names.index(name)
            pairs = [p for i, p in enumerate(pairs) if p[0] != name or i == first]
            pairs[first] = (name, value)
        else:
            pairs.append((name, value))
    serialized = "&".join(f"{form_enc(n)}={form_enc(v)}" for n, v in pairs)
    return f"{base}{'?' if serialized else ''}{serialized}{fragment}"


def parse_retry_after(value: str | None, now: float) -> int | None:
    """Retry-After in milliseconds (delta seconds or HTTP date), or None."""
    if value is None:
        return None
    trimmed = value.strip()
    if _DIGITS.fullmatch(trimmed) is not None:
        return int(trimmed) * 1000
    try:
        when = parsedate_to_datetime(trimmed)
    except (TypeError, ValueError, IndexError):
        try:
            when = dt.datetime.fromisoformat(trimmed)
        except ValueError:
            return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=dt.UTC)
    return max(0, math.floor(when.timestamp() * 1000 - now + 0.5))


_LINK_SPLIT = re.compile(r",(?=\s*<)")
_LINK_PART = re.compile(r"\s*<([^>]*)>(.*)")
_REL_NEXT = re.compile(r';\s*rel\s*=\s*"?([^";]*\s)?next(\s[^";]*)?"?\s*(;|$)', re.IGNORECASE)


def next_link(header: str | None) -> str | None:
    """Next page URL from a ``Link`` header (``rel="next"``), or None."""
    if not header:
        return None
    for part in _LINK_SPLIT.split(header):
        match = _LINK_PART.fullmatch(part)
        if match is None:
            continue
        if _REL_NEXT.search(match.group(2)) is not None:
            return match.group(1)
    return None


def json_ready(value: object, depth: int = 0) -> object:
    """``UNSET`` inside arrays becomes None (``undefined`` serializes as null)."""
    if depth > 64:
        return value
    if is_array(value):
        return [None if item is UNSET else json_ready(item, depth + 1) for item in value]
    if is_record(value):
        return {k: json_ready(v, depth + 1) for k, v in value.items() if v is not UNSET}
    return value
