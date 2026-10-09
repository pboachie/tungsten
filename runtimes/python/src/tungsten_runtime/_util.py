# SPDX-License-Identifier: Apache-2.0
"""Small pure helpers shared by the runtime modules: value inspection,
field paths, hashing, encodings, redaction and argument normalization
(the counterpart of ``runtimes/ts/src/util.ts``).

``UNSET`` plays the part of JavaScript's ``undefined`` throughout: a path
that does not resolve reads as ``UNSET``, and ``UNSET`` members are absent.
"""

from __future__ import annotations

import base64
import datetime as dt
import hashlib
import hmac
import math
import os
import re
from collections.abc import Mapping, Sequence
from types import MappingProxyType
from typing import TypeGuard

from ._json import FilePart, display_json, is_bytes_like, js_number, parse_json
from .sentinels import UNSET

#: Placeholder written wherever a secret or sensitive value would appear.
REDACTED = "<redacted>"

#: Longest string kept in ``Diagnostic.received_value``.
MAX_RECEIVED_CHARS = 200

_DIGITS = re.compile(r"[0-9]+")


def is_record(value: object) -> TypeGuard[Mapping[str, object]]:
    """A JSON object (any mapping)."""
    return isinstance(value, Mapping)


def is_array(value: object) -> TypeGuard[Sequence[object]]:
    """A JSON array (a list or a tuple)."""
    return isinstance(value, list | tuple)


def is_number(value: object) -> TypeGuard[int | float]:
    """A JavaScript number: an int or a float, never a bool."""
    return isinstance(value, int | float) and not isinstance(value, bool)


#: An empty mapping (shared, read-only).
NOTHING: Mapping[str, object] = MappingProxyType({})


def items_of(value: object) -> Sequence[object]:
    """The elements of a JSON array, or none."""
    return value if is_array(value) else ()


def entries_of(value: object) -> Mapping[str, object]:
    """The members of a JSON object, or none."""
    return value if is_record(value) else NOTHING


def is_binary(value: object) -> TypeGuard[bytes | bytearray | memoryview | FilePart]:
    return isinstance(value, bytes | bytearray | memoryview | FilePart)


def binary_size(value: bytes | bytearray | memoryview | FilePart) -> int:
    return len(value.content) if isinstance(value, FilePart) else len(bytes(value))


def field(value: object, key: str) -> object:
    """``value[key]`` of a mapping, or ``UNSET``; never raises."""
    if not is_record(value):
        return UNSET
    try:
        return value.get(key, UNSET)
    except Exception:
        return UNSET


def str_field(value: object, key: str) -> str | None:
    found = field(value, key)
    return found if isinstance(found, str) else None


def split_path(path: str) -> list[str]:
    """Split a field path (``a.b``, ``items[0].id``, ``items.0.id``) into segments."""
    if path in ("", "."):
        return []
    return [s for s in re.sub(r"\[([0-9]+)\]", r".\1", path).split(".") if s != ""]


def get_path(value: object, path: str | Sequence[str]) -> object:
    """Read a field path from a JSON-like value; ``UNSET`` when absent."""
    segments = split_path(path) if isinstance(path, str) else path
    current = value
    for segment in segments:
        if is_array(current):
            if _DIGITS.fullmatch(segment) is None:
                return UNSET
            index = int(segment)
            if index >= len(current):
                return UNSET
            current = current[index]
        elif is_record(current):
            if segment not in current:
                return UNSET
            current = current[segment]
        else:
            return UNSET
    return current


def deep_equal(a: object, b: object) -> bool:
    """Structural equality over JSON-like values (a bool never equals a number)."""
    if a is b:
        return True
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if is_number(a) and is_number(b):
        return a == b
    if isinstance(a, str) and isinstance(b, str):
        return a == b
    if is_array(a):
        return is_array(b) and len(a) == len(b) and all(deep_equal(x, y) for x, y in zip(a, b, strict=True))
    if is_record(a) and is_record(b):
        ka = [k for k in a if a[k] is not UNSET]
        kb = [k for k in b if b[k] is not UNSET]
        return len(ka) == len(kb) and all(k in b and deep_equal(a[k], b[k]) for k in ka)
    if a is None or b is None or a is UNSET or b is UNSET:
        return False
    if is_binary(a) and is_binary(b):
        return a == b
    return False


def contains_subset(value: object, pattern: object) -> bool:
    """Every member of ``pattern`` appears in ``value`` with an equal
    (recursively contained) value; array patterns match when each element is
    contained by some element of the corresponding array."""
    if is_record(pattern):
        if not is_record(value):
            return False
        return all(k in value and contains_subset(value[k], pattern[k]) for k in pattern)
    if is_array(pattern):
        if not is_array(value):
            return False
        return all(any(contains_subset(v, p) for v in value) for p in pattern)
    return deep_equal(value, pattern)


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def _has_surrogate(text: str) -> bool:
    return any(0xD800 <= ord(c) <= 0xDFFF for c in text)


def usv(text: str) -> str:
    """The string as a sequence of Unicode scalar values: surrogate pairs
    joined, lone surrogates replaced by U+FFFD (WHATWG ``USVString``)."""
    if not _has_surrogate(text):
        return text
    return text.encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace")


def utf8(text: str) -> bytes:
    """UTF-8 of a string; lone surrogates become U+FFFD, as ``TextEncoder`` does."""
    return usv(text).encode("utf-8")


def sha256_hex(data: str | bytes) -> str:
    """Lowercase hex SHA-256 of UTF-8 text or bytes."""
    return hashlib.sha256(utf8(data) if isinstance(data, str) else data).hexdigest()


def hmac_sha256(key: bytes, message: str) -> bytes:
    return hmac.new(key, utf8(message), hashlib.sha256).digest()


def random_bytes(n: int) -> bytes:
    return os.urandom(n)


_SENSITIVE_NAME = re.compile(
    r"(secret|password|passwd|passphrase|token|api[-_]?key|private[-_]?key|credential|authorization|cookie|session)",
    re.IGNORECASE,
)


def looks_sensitive(name: str) -> bool:
    """Names that very likely hold a secret, redacted even without metadata."""
    return _SENSITIVE_NAME.search(name) is not None


def redact_sensitive_keys(value: object, depth: int = 0) -> object:
    """Deep copy of a JSON-like value with sensitive-looking keys redacted."""
    if depth > 32:
        return None
    if is_array(value):
        return [redact_sensitive_keys(v, depth + 1) for v in value]
    if is_record(value):
        return {
            k: REDACTED if looks_sensitive(k) else redact_sensitive_keys(v, depth + 1)
            for k, v in value.items()
        }
    return value


def redact_paths(value: object, paths: Sequence[str]) -> object:
    """Deep copy with every listed field path redacted; arrays on the way fan
    out, so ``items.secret`` redacts the member in each item."""
    if len(paths) == 0:
        return value

    def apply(v: object, segments: Sequence[str]) -> object:
        if len(segments) == 0:
            return REDACTED
        if is_array(v):
            return [apply(item, segments) for item in v]
        if not is_record(v):
            return v
        head, rest = segments[0], segments[1:]
        if head not in v:
            return v
        copy = dict(v)
        copy[head] = apply(v[head], rest)
        return copy

    out = value
    for path in paths:
        segments = split_path(path)
        if len(segments) > 0:
            out = apply(out, segments)
    return out


def cut(text: str) -> str:
    return f"{text[: MAX_RECEIVED_CHARS - 1]}…" if len(text) > MAX_RECEIVED_CHARS else text


def envelope_value(value: object, sensitive: bool) -> object:
    """A value made safe for ``Diagnostic.received_value``: redacted when
    sensitive, binary summarised, strings and large values cut to 200
    characters, sensitive-looking nested keys redacted."""
    if sensitive:
        return REDACTED
    if value is UNSET or value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return cut(value)
    if isinstance(value, int):
        return int(value)
    if isinstance(value, float):
        return value if math.isfinite(value) else js_number(value)
    if is_binary(value):
        return f"<{binary_size(value)} bytes>"
    if not (is_record(value) or is_array(value) or isinstance(value, dt.date)):
        return f"<{type(value).__name__}>"
    text = display_json(redact_sensitive_keys(value))
    if len(text) > MAX_RECEIVED_CHARS:
        return cut(text)
    try:
        return parse_json(text)
    except ValueError:
        return cut(text)


def describe_error(error: BaseException) -> str:
    """A short, secret-free description of an exception."""
    message = str(error)
    name = type(error).__name__
    return cut(f"{name}: {message}" if message else name)


def bounded(value: object, fallback: float, low: float = 0, high: float = float(2**53 - 1)) -> float:
    """A finite number in [low, high], or the fallback."""
    if is_number(value) and math.isfinite(value):
        return min(high, max(low, float(value)))
    return fallback


# ------------------------------------------------------- argument values


def without_unset(value: object, depth: int = 0) -> object:
    """A copy of the arguments without ``UNSET`` members at any depth: an
    optional argument left ``UNSET`` is absent. Only mappings and lists are
    copied; other values are kept as they are."""
    if depth > 64:
        return value
    if is_array(value):
        # A tuple is kept whole: it can be a multipart file part.
        return value if isinstance(value, tuple) else [without_unset(item, depth + 1) for item in value]
    if is_record(value):
        return {k: without_unset(v, depth + 1) for k, v in value.items() if v is not UNSET}
    return value


def has_cycle(value: object) -> bool:
    """Whether a mapping or list contains itself."""
    stack: set[int] = set()

    def visit(v: object, depth: int) -> bool:
        if depth > 512 or not (is_record(v) or is_array(v)):
            return False
        marker = id(v)
        if marker in stack:
            return True
        stack.add(marker)
        try:
            children = list(v.values()) if is_record(v) else list(v)  # pyright: ignore[reportArgumentType]
            return any(visit(child, depth + 1) for child in children)
        finally:
            stack.discard(marker)

    return visit(value, 0)


def _read_file(handle: object) -> FilePart | None:
    read = getattr(handle, "read", None)
    if not callable(read):
        return None
    seek = getattr(handle, "seek", None)
    tell = getattr(handle, "tell", None)
    start = tell() if callable(tell) else None
    data = read()
    if callable(seek) and isinstance(start, int):
        seek(start)
    if isinstance(data, str):
        data = utf8(data)
    if not is_bytes_like(data):
        raise TypeError("a file object must return bytes or text from read()")
    name = getattr(handle, "name", None)
    filename = os.path.basename(name) if isinstance(name, str) and name != "" else None
    return FilePart(filename, bytes(data), None)


def _file_tuple(value: Sequence[object]) -> FilePart | None:
    if not isinstance(value, tuple) or len(value) not in (2, 3):
        return None
    filename = value[0]
    content = value[1]
    content_type = value[2] if len(value) == 3 else None
    if not (filename is None or isinstance(filename, str)) or not (
        content_type is None or isinstance(content_type, str)
    ):
        return None
    if is_bytes_like(content):
        return FilePart(filename, bytes(content), content_type)
    read = _read_file(content) if not isinstance(content, str) else None
    if read is None:
        return None
    return FilePart(filename if filename is not None else read.filename, read.content, content_type)


def normalize_files(value: object, depth: int = 0) -> object:
    """File objects and ``(filename, content[, content_type])`` tuples
    become ``FilePart`` values (read once; a seekable file is rewound to
    where it was), so every later step digests and sends the same bytes."""
    if depth > 64 or isinstance(value, str | FilePart) or is_bytes_like(value):
        return value
    if is_record(value):
        return {k: normalize_files(v, depth + 1) for k, v in value.items()}
    if is_array(value):
        part = _file_tuple(value)
        if part is not None:
            return part
        return [normalize_files(v, depth + 1) for v in value]
    part = _read_file(value)
    return part if part is not None else value


_SUPPORTED_SCALARS = (str, int, float, bool, bytes, bytearray, memoryview, FilePart, dt.date)


def unsupported_path(value: object, path: list[str | int] | None = None) -> list[str | int] | None:
    """The path of the first value that is not JSON-like (nor binary), or None."""
    here: list[str | int] = path if path is not None else []
    if value is None or value is UNSET or isinstance(value, _SUPPORTED_SCALARS):
        return None
    if is_record(value):
        for key, item in value.items():
            if not isinstance(key, str):  # pyright: ignore[reportUnnecessaryIsInstance]
                return here
            found = unsupported_path(item, [*here, key])
            if found is not None:
                return found
        return None
    if is_array(value):
        for index, item in enumerate(value):
            found = unsupported_path(item, [*here, index])
            if found is not None:
                return found
        return None
    return here


def merge_accept(value: str) -> str:
    """The ``Accept`` value of a stream call: the caller's media ranges in
    order without repeats, ``text/event-stream`` first when they do not name
    it."""
    ranges: list[str] = []
    for part in value.split(","):
        media_range = part.strip()
        if media_range != "" and media_range not in ranges:
            ranges.append(media_range)
    names = any(r.split(";", 1)[0].strip().lower() == "text/event-stream" for r in ranges)
    return ", ".join(ranges if names else ["text/event-stream", *ranges])
