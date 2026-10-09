# SPDX-License-Identifier: Apache-2.0
"""JSON text written exactly as the TypeScript runtime writes it.

Two encodings share one encoder:

- ``json_text`` is ``JSON.stringify``: members in insertion order, no
  whitespace. Request bodies, remediation text and previews use it.
- ``canonical_json`` is the canonical form every tungsten runtime digests
  (confirmation tokens, ``auto`` idempotency logical ids, ``content_hash``
  keys). The rule, identical to ``canonicalJson`` in
  ``runtimes/ts/src/util.ts`` byte for byte:

  * object members sorted by the UTF-16 code units of their names (not by
    code points: U+1F600 sorts before U+FF61), no whitespace;
  * an absent value (``UNSET``) is dropped from objects and is ``null`` in
    arrays; ``None`` is ``null``;
  * numbers as ECMAScript ``Number.prototype.toString`` writes them
    (``1.0`` is ``1``, ``1e-7`` is ``1e-7``, ``1e21`` is ``1e+21``, ``-0``
    is ``0``), non-finite numbers as ``null``; integers are written exactly,
    so digests agree across runtimes for integers within +-2**53, the range
    a JavaScript number holds exactly;
  * strings as ``JSON.stringify`` escapes them: ``"`` and ``\\``, ``\\b \\f
    \\n \\r \\t``, other control characters as lowercase ``\\u00xx``, lone
    surrogates as lowercase ``\\udxxx``, everything else as is (UTF-8);
  * ``datetime`` as ``toISOString`` writes a ``Date``
    (``2026-01-02T03:04:05.000Z``, UTC, milliseconds; a naive value is
    taken as UTC), ``date`` as ``YYYY-MM-DD``;
  * binary values as ``{"$bytes": <base64>}``; a file part (a multipart
    file given as a file object or a ``(filename, content, content_type)``
    tuple) as ``{"$file": {"bytes": <base64>, "name": ..., "type": ...}}``;
  * tuples are arrays; a cycle or any other type raises ``TypeError``.
"""

from __future__ import annotations

import base64
import datetime as dt
import json
import math
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TypeGuard

from .sentinels import UNSET


@dataclass(frozen=True, slots=True)
class FilePart:
    """One file of a multipart body, read into memory once."""

    filename: str | None
    content: bytes
    content_type: str | None


def is_bytes_like(value: object) -> TypeGuard[bytes | bytearray | memoryview[int]]:
    return isinstance(value, bytes | bytearray | memoryview)


_SURROGATE = re.compile("[\ud800-\udfff]")


def js_string(text: str) -> str:
    """A string literal as ``JSON.stringify`` writes it."""
    encoded = json.dumps(text, ensure_ascii=False)
    if _SURROGATE.search(encoded) is None:
        return encoded
    out: list[str] = []
    i = 0
    while i < len(encoded):
        code = ord(encoded[i])
        if 0xD800 <= code <= 0xDBFF and i + 1 < len(encoded) and 0xDC00 <= ord(encoded[i + 1]) <= 0xDFFF:
            out.append(chr(0x10000 + ((code - 0xD800) << 10) + (ord(encoded[i + 1]) - 0xDC00)))
            i += 2
            continue
        out.append(f"\\u{code:04x}" if 0xD800 <= code <= 0xDFFF else encoded[i])
        i += 1
    return "".join(out)


def js_number(value: float) -> str:
    """``String(value)`` for a JavaScript number (ECMAScript Number::toString)."""
    if math.isnan(value):
        return "NaN"
    if math.isinf(value):
        return "Infinity" if value > 0 else "-Infinity"
    if value == 0:
        return "0"
    sign = "-" if value < 0 else ""
    text = repr(abs(value))
    mantissa, _, exponent_text = text.partition("e")
    exponent = int(exponent_text) if exponent_text else 0
    integer, _, fraction = mantissa.partition(".")
    digits = integer + fraction
    point = len(integer) + exponent
    stripped = digits.lstrip("0")
    point -= len(digits) - len(stripped)
    digits = stripped.rstrip("0") or "0"
    k = len(digits)
    n = point
    if k <= n <= 21:
        body = digits + "0" * (n - k)
    elif 0 < n <= 21:
        body = f"{digits[:n]}.{digits[n:]}"
    elif -6 < n <= 0:
        body = f"0.{'0' * -n}{digits}"
    else:
        e = n - 1
        mark = "+" if e >= 0 else "-"
        head = digits if k == 1 else f"{digits[0]}.{digits[1:]}"
        body = f"{head}e{mark}{abs(e)}"
    return sign + body


def iso_datetime(value: dt.datetime) -> str:
    """``Date.prototype.toISOString`` of a datetime (UTC, milliseconds)."""
    utc = value.astimezone(dt.UTC) if value.tzinfo is not None else value
    return f"{utc.year:04d}-{utc.month:02d}-{utc.day:02d}T{utc.hour:02d}:{utc.minute:02d}:{utc.second:02d}.{utc.microsecond // 1000:03d}Z"


def _utf16_key(name: str) -> bytes:
    return name.encode("utf-16-be", "surrogatepass")


def _b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


class _Encoder:
    __slots__ = ("binary", "lenient", "sort", "stack")

    def __init__(self, *, sort: bool, binary: bool, lenient: bool) -> None:
        self.sort = sort
        self.binary = binary
        self.lenient = lenient
        self.stack: set[int] = set()

    def unsupported(self, what: str) -> str:
        if self.lenient:
            return js_string(f"<{what}>")
        raise TypeError(f"{what} is not JSON-serializable")

    def encode(self, value: object) -> str | None:
        if value is UNSET:
            return None
        if value is None:
            return "null"
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, int):
            return int.__repr__(value)
        if isinstance(value, float):
            return js_number(value) if math.isfinite(value) else "null"
        if isinstance(value, str):
            return js_string(str.__str__(value))
        if isinstance(value, dt.datetime):
            return js_string(iso_datetime(value))
        if isinstance(value, dt.date):
            return js_string(value.isoformat())
        if is_bytes_like(value):
            if self.binary:
                return '{"$bytes":' + js_string(_b64(bytes(value))) + "}"
            return self.unsupported(f"{len(bytes(value))} bytes")
        if isinstance(value, FilePart):
            if self.binary:
                name = "null" if value.filename is None else js_string(value.filename)
                kind = "null" if value.content_type is None else js_string(value.content_type)
                return f'{{"$file":{{"bytes":{js_string(_b64(value.content))},"name":{name},"type":{kind}}}}}'
            return self.unsupported(f"{len(value.content)} bytes")
        if isinstance(value, Mapping | list | tuple):
            return self.container(value)  # pyright: ignore[reportUnknownArgumentType]
        return self.unsupported(type(value).__name__)

    def container(self, value: Mapping[object, object] | list[object] | tuple[object, ...]) -> str:
        marker = id(value)
        if marker in self.stack:
            if self.lenient:
                return js_string("<cycle>")
            raise TypeError("the value contains a cycle")
        self.stack.add(marker)
        try:
            if isinstance(value, Mapping):
                names: list[str] = []
                for key in value:
                    if not isinstance(key, str):
                        if self.lenient:
                            continue
                        raise TypeError("object member names must be strings")
                    names.append(key)
                if self.sort:
                    names.sort(key=_utf16_key)
                parts: list[str] = []
                for key in names:
                    encoded = self.encode(value[key])
                    if encoded is not None:
                        parts.append(f"{js_string(key)}:{encoded}")
                return "{" + ",".join(parts) + "}"
            return "[" + ",".join(self.encode(item) or "null" for item in value) + "]"
        finally:
            self.stack.discard(marker)


def canonical_json(value: object) -> str:
    """The canonical JSON of a value (see the module documentation)."""
    return _Encoder(sort=True, binary=True, lenient=False).encode(value) or "null"


def json_text(value: object) -> str:
    """``JSON.stringify(value)`` for a JSON-ready value; ``TypeError`` for
    anything else (binary data, cycles, an absent top-level value)."""
    encoded = _Encoder(sort=False, binary=False, lenient=False).encode(value)
    if encoded is None:
        raise TypeError("an absent value is not JSON-serializable")
    return encoded


def display_json(value: object) -> str:
    """JSON for messages: like ``json_text``, never raising (binary data and
    other types are written as ``"<...>"`` descriptions)."""
    return _Encoder(sort=False, binary=False, lenient=True).encode(value) or "null"


def _reject_constant(name: str) -> object:
    raise ValueError(f"{name} is not JSON")


def parse_json(text: str) -> object:
    """``JSON.parse``: strict JSON (no NaN or Infinity); ``ValueError`` otherwise."""
    return json.loads(text, parse_constant=_reject_constant)
