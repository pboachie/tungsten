# SPDX-License-Identifier: Apache-2.0
"""Parameter serialization (OpenAPI 3 ``style``/``explode`` for path, query,
header and cookie parameters) and request body encoding
(``runtimes/ts/src/serialize.ts``)."""

from __future__ import annotations

import datetime as dt
import re
from collections.abc import Callable, Collection, Mapping, Sequence
from dataclasses import dataclass
from urllib.parse import quote

import httpx

from ._json import FilePart, canonical_json, iso_datetime, js_number, json_text, parse_json
from ._util import REDACTED, binary_size, is_array, is_binary, is_record, usv, utf8
from .sentinels import UNSET
from .types import BodyDescriptor, OperationDescriptor, ParamDescriptor


class SerializationError(Exception):
    """A value the encoding cannot carry; becomes VALIDATION_FAILED."""

    def __init__(self, parameter: str, expected: str, value: object) -> None:
        super().__init__(f"{parameter}: expected {expected}")
        self.parameter = parameter
        self.expected = expected
        self.value = value


def scalar(value: object) -> str:
    """``String(value)`` of a parameter value (JSON for objects)."""
    if isinstance(value, str):
        return value
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return int.__repr__(value)
    if isinstance(value, float):
        return js_number(value)
    if isinstance(value, dt.datetime):
        return iso_datetime(value)
    if isinstance(value, dt.date):
        return value.isoformat()
    try:
        return json_text(value)
    except TypeError:
        return ""


def enc(text: str) -> str:
    """``encodeURIComponent``."""
    return quote(usv(text), safe="!'()*-._~")


def form_enc(text: str) -> str:
    """One name or value as ``URLSearchParams`` serializes it."""
    return quote(usv(text), safe="* ").replace(" ", "+").replace("~", "%7E")


def _present(value: object) -> bool:
    return value is not None and value is not UNSET


def _entries(value: Mapping[str, object]) -> list[tuple[str, object]]:
    return [(k, v) for k, v in value.items() if _present(v)]


def serialize_path_param(p: ParamDescriptor, value: object) -> str:
    """One path parameter value, already percent-encoded (simple, label, matrix)."""
    name = enc(p["wire"])
    style = p["style"] if p["style"] in ("label", "matrix") else "simple"
    explode = p["explode"] is True
    if is_array(value):
        items = [enc(scalar(v)) for v in value if _present(v)]
        if style == "label":
            return "." + ("." if explode else ",").join(items)
        if style == "matrix":
            return "".join(f";{name}={i}" for i in items) if explode else f";{name}={','.join(items)}"
        return ",".join(items)
    if is_record(value):
        pairs = [(enc(k), enc(scalar(v))) for k, v in _entries(value)]
        exploded = [f"{k}={v}" for k, v in pairs]
        flat = [part for pair in pairs for part in pair]
        if style == "label":
            return "." + ".".join(exploded) if explode else "." + ",".join(flat)
        if style == "matrix":
            return "".join(f";{e}" for e in exploded) if explode else f";{name}={','.join(flat)}"
        return ",".join(exploded) if explode else ",".join(flat)
    v = enc(scalar(value))
    if style == "label":
        return f".{v}"
    if style == "matrix":
        return f";{name}={v}"
    return v


def _deep_object_pairs(prefix: str, value: object, out: list[str], depth: int) -> None:
    if depth > 16 or not _present(value):
        return
    if is_array(value):
        for item in value:
            _deep_object_pairs(prefix, item, out, depth + 1)
    elif is_record(value):
        for k, v in _entries(value):
            _deep_object_pairs(f"{prefix}[{enc(k)}]", v, out, depth + 1)
    else:
        out.append(f"{prefix}={enc(scalar(value))}")


def serialize_query_param(p: ParamDescriptor, value: object) -> list[str]:
    """Query string parts (``name=value``, encoded) for one query parameter."""
    if not _present(value):
        return []
    name = enc(p["wire"])
    style = p["style"]
    delimiter = "%20" if style == "space_delimited" else "|" if style == "pipe_delimited" else ","
    if style == "deep_object" and is_record(value):
        out: list[str] = []
        _deep_object_pairs(name, value, out, 0)
        return out
    if is_array(value):
        items = [enc(scalar(v)) for v in value if _present(v)]
        if len(items) == 0:
            return []
        if p["explode"] is True or style == "deep_object":
            return [f"{name}={i}" for i in items]
        return [f"{name}={delimiter.join(items)}"]
    if is_record(value):
        pairs = [(enc(k), enc(scalar(v))) for k, v in _entries(value)]
        if p["explode"] is True:
            return [f"{k}={v}" for k, v in pairs]
        return [f"{name}={delimiter.join(part for pair in pairs for part in pair)}"]
    return [f"{name}={enc(scalar(value))}"]


def serialize_header_param(p: ParamDescriptor, value: object) -> str:
    """Header value for one header parameter (``simple`` style, not encoded)."""
    if is_array(value):
        return ",".join(scalar(v) for v in value if _present(v))
    if is_record(value):
        pairs = [(k, scalar(v)) for k, v in _entries(value)]
        if p["explode"] is True:
            return ",".join(f"{k}={v}" for k, v in pairs)
        return ",".join(part for pair in pairs for part in pair)
    return scalar(value)


def serialize_cookie_param(p: ParamDescriptor, value: object) -> str:
    """``name=value`` for one cookie parameter (``form`` style, values encoded)."""
    if is_array(value):
        return f"{p['wire']}={','.join(enc(scalar(v)) for v in value if _present(v))}"
    if is_record(value):
        return (
            f"{p['wire']}={','.join(part for k, v in _entries(value) for part in (enc(k), enc(scalar(v))))}"
        )
    return f"{p['wire']}={enc(scalar(value))}"


@dataclass(frozen=True, slots=True)
class EncodedBody:
    """An encoded request body."""

    #: What is sent; None for no body.
    body: bytes | None
    #: Content-Type to set, or None.
    content_type: str | None
    #: JSON-friendly rendering for previews (binary summarised), redacted.
    display: object
    #: The same rendering before redaction (never shown).
    raw: object
    #: Bytes or text whose SHA-256 is the ``content_hash`` key.
    hash_material: str | bytes | None


NO_BODY = EncodedBody(None, None, None, None, None)


def body_value(op: OperationDescriptor, args: Mapping[str, object], param_names: Collection[str]) -> object:
    """The body value before encoding: merged fields picked from the args
    (sent under their wire names), or the whole ``args[arg]``; wrapped in the
    rpc envelope when the operation has one. ``UNSET`` means no body."""
    value: object = UNSET
    body = op["body"]
    rpc = op["rpc"]
    if body is not None:
        shape = body["shape"]
        if shape["kind"] == "merged":
            picked: dict[str, object] = {}
            for merged in shape["fields"]:
                if merged["arg"] in args and args[merged["arg"]] is not UNSET:
                    picked[merged["wire"]] = args[merged["arg"]]
            value = picked if len(picked) > 0 or body["required"] is True or rpc is not None else UNSET
        else:
            value = args.get(shape["arg"], UNSET)
    if rpc is not None:
        params = value
        if body is None:
            params = {k: v for k, v in args.items() if k not in param_names and v is not UNSET}
        envelope: dict[str, object] = dict(rpc.get("constants") or {})
        envelope[rpc["field"]] = rpc["value"]
        envelope[rpc["params_field"]] = {} if params is UNSET else params
        return envelope
    return value


def form_pairs(value: object, parameter: str) -> list[tuple[str, str]]:
    if not is_record(value):
        raise SerializationError(parameter, "a mapping of form fields", value)
    out: list[tuple[str, str]] = []
    for k, v in _entries(value):
        if is_array(v):
            out.extend((k, scalar(item)) for item in v if _present(item))
        else:
            out.append((k, scalar(v)))
    return out


def form_text(pairs: Sequence[tuple[str, str]]) -> str:
    return "&".join(f"{form_enc(k)}={form_enc(v)}" for k, v in pairs)


def _binary_display(value: bytes | bytearray | memoryview | FilePart, media_type: str) -> str:
    return f"<{binary_size(value)} bytes{f' of {media_type}' if media_type else ''}>"


def _file_of(value: bytes | bytearray | memoryview | FilePart, name: str) -> FilePart:
    if isinstance(value, FilePart):
        return value
    return FilePart(name, bytes(value), None)


def encode_body(
    descriptor: BodyDescriptor | None, value: object, redact_display: Callable[[object], object]
) -> EncodedBody:
    """Encode ``value`` per the body descriptor. Raises ``SerializationError``
    for values the encoding cannot carry."""
    if value is UNSET:
        return NO_BODY
    encoding = descriptor["encoding"] if descriptor is not None else "json"
    media_type = descriptor["media_type"] if descriptor is not None else "application/json"
    parameter = "body"
    if encoding == "json":
        try:
            text = json_text(value)
        except (TypeError, RecursionError) as error:
            raise SerializationError(parameter, "a JSON-serializable value", value) from error
        raw = parse_json(text)
        return EncodedBody(utf8(text), media_type, redact_display(raw), raw, canonical_json(raw))
    if encoding == "form":
        pairs = form_pairs(value, parameter)
        text = form_text(pairs)
        raw = dict(pairs)
        return EncodedBody(text.encode("ascii"), media_type, redact_display(raw), raw, text)
    if encoding == "multipart":
        if not is_record(value):
            raise SerializationError(parameter, "a mapping of multipart fields", value)
        files: list[tuple[str, tuple[str | None, bytes, str | None]]] = []
        display: dict[str, object] = {}
        for k, v in _entries(value):
            items = [item for item in v if _present(item)] if is_array(v) else [v]
            shown: list[object] = []
            for item in items:
                if is_binary(item):
                    part = _file_of(item, k)
                    content_type = part.content_type or "application/octet-stream"
                    files.append(
                        (k, (part.filename if part.filename is not None else k, part.content, content_type))
                    )
                    shown.append(_binary_display(item, part.content_type or ""))
                elif is_record(item):
                    try:
                        files.append((k, (None, utf8(json_text(item)), "application/json")))
                    except (TypeError, RecursionError) as error:
                        raise SerializationError(
                            parameter, "JSON-serializable multipart fields", item
                        ) from error
                    shown.append(item)
                else:
                    files.append((k, (None, utf8(scalar(item)), None)))
                    shown.append(scalar(item))
            display[k] = shown if is_array(v) else shown[0] if len(shown) > 0 else None
        request = httpx.Request("POST", "http://multipart.invalid/", files=files)
        content = request.read()
        return EncodedBody(
            content,
            request.headers.get("content-type"),
            redact_display(display),
            display,
            canonical_json(value),
        )
    if encoding == "bytes":
        if is_binary(value):
            material = value.content if isinstance(value, FilePart) else bytes(value)
        elif isinstance(value, str):
            material = utf8(value)
        else:
            raise SerializationError(
                parameter, "binary data (bytes, bytearray, memoryview or a binary file)", value
            )
        shown_bytes = _binary_display(material, media_type)
        return EncodedBody(material, media_type, shown_bytes, shown_bytes, material)
    if isinstance(value, str | bool | int | float):
        text = scalar(value)
        return EncodedBody(utf8(text), media_type, text, text, text)
    raise SerializationError(parameter, "a string", value)


_TOKEN = re.compile(r"[!#$%&'*+\-.^_`|~0-9A-Za-z]+")
_HEADER_VALUE = re.compile(r"[\t\x20-\x7e\x80-\xff]*")


def valid_header_name(name: str) -> bool:
    return _TOKEN.fullmatch(name) is not None


def valid_header_value(value: str) -> bool:
    return _HEADER_VALUE.fullmatch(value) is not None


@dataclass(slots=True)
class _HeaderEntry:
    name: str
    value: str
    secret: bool


class HeaderBag:
    """Case-insensitive header collection that remembers which values are
    secrets, so previews and middleware see them redacted."""

    __slots__ = ("_entries",)

    def __init__(self) -> None:
        self._entries: dict[str, _HeaderEntry] = {}

    def set(self, name: str, value: str, secret: bool = False) -> None:
        self._entries[name.lower()] = _HeaderEntry(name, value, secret)

    def get(self, name: str) -> str | None:
        entry = self._entries.get(name.lower())
        return entry.value if entry is not None else None

    def has(self, name: str) -> bool:
        return name.lower() in self._entries

    def is_secret(self, name: str) -> bool:
        entry = self._entries.get(name.lower())
        return entry.secret if entry is not None else False

    def to_record(self, redact: bool) -> dict[str, str]:
        """Plain mapping; secret values replaced by ``<redacted>`` when ``redact``."""
        return {e.name: REDACTED if redact and e.secret else e.value for e in self._entries.values()}

    def secrets(self) -> list[str]:
        return [e.value for e in self._entries.values() if e.secret]


def display_form(display: object) -> bytes:
    """The form rendering middleware sees when redaction changed a form body."""
    if not is_record(display):
        return b""
    pairs: list[tuple[str, str]] = []
    for k, v in display.items():
        pairs.append((k, v if isinstance(v, str) else json_text(v) if _present(v) else "null"))
    return form_text(pairs).encode("ascii")


def parse_json_bytes(data: bytes) -> object:
    return parse_json(data.decode("utf-8-sig", "replace"))
