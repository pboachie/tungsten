# SPDX-License-Identifier: Apache-2.0
"""Dataclass models and their validation: what a Python SDK generated with
``models: dataclasses`` builds on, so the package needs no dependency beyond
this runtime.

A generated model is a stdlib dataclass (``kw_only``) decorated with
``model``; its field annotations carry the constraints as ``Annotated``
metadata (``Limits``, ``pattern``, ``string_format``, ``exact``, ``one_of``,
``all_of``, ``Tag``/``tag``). ``Request`` and ``Response`` are the
``Validator`` implementations the operation descriptors name: they read the
annotations once, lazily (so models may refer to each other in any order),
and validate strictly, with the rules the Pydantic flavor of the SDK has: no
coercion between JSON types (``"1"`` is not an integer; an integral number
such as ``1.0`` is), no ``NaN`` or infinity, unknown members refused by
closed records, and the same issue messages and codes for the common
failures.

Three modes read a value. ``response`` builds the typed value (model
instances); ``json`` builds the JSON-ready wire value of a request argument
(wire names, ``UNSET`` removed, bytes as base64); ``plain`` is the wire value
of a form, multipart or binary body, where bytes and files stay as they are
for the runtime to encode.
"""

from __future__ import annotations

import base64
import binascii
import dataclasses
import math
import re
from collections.abc import Callable, Mapping, Sequence
from types import NoneType, UnionType
from typing import (
    Annotated,
    Any,
    Literal,
    Self,
    TypeAliasType,
    Union,
    cast,
    dataclass_transform,
    get_args,
    get_origin,
    get_type_hints,
)

from .sentinels import UNSET
from .types import Invalid, Issue, Valid

Mode = Literal["response", "json", "plain"]
Path = list[str | int]

#: The attribute that holds the extra members of a record that admits them.
EXTRA_ATTRIBUTE = "model_extra"
_EXTRA_POLICY = "__tungsten_extra__"


class ValidationError(ValueError):
    """A value failed validation; ``issues`` says where and why."""

    def __init__(self, issues: list[Issue]) -> None:
        self.issues = issues
        first = issues[0] if issues else None
        text = "validation failed"
        if first is not None:
            where = ".".join(str(p) for p in first["path"]) or "value"
            text = f"{where}: {first['message']}"
            if len(issues) > 1:
                text += f" (and {len(issues) - 1} more)"
        super().__init__(text)


# ------------------------------------------------------------- metadata


@dataclasses.dataclass(frozen=True, slots=True)
class Limits:
    """Length (``str``, ``list``) and numeric bounds of a value."""

    min_length: int | None = None
    max_length: int | None = None
    ge: int | float | None = None
    le: int | float | None = None
    gt: int | float | None = None
    lt: int | float | None = None
    multiple_of: int | float | None = None


@dataclasses.dataclass(frozen=True, slots=True)
class Tag:
    """Labels a member of a tagged union with its tag value."""

    value: str


@dataclasses.dataclass(frozen=True, slots=True)
class TagReader:
    """The discriminator of a tagged union."""

    wire: str
    aliases: tuple[tuple[str, str], ...] = ()


@dataclasses.dataclass(frozen=True, slots=True)
class _Pattern:
    source: str


@dataclasses.dataclass(frozen=True, slots=True)
class _Format:
    name: str


@dataclasses.dataclass(frozen=True, slots=True)
class _Exact:
    values: tuple[object, ...]


@dataclasses.dataclass(frozen=True, slots=True)
class _OneOf:
    values: tuple[object, ...]


@dataclasses.dataclass(frozen=True, slots=True)
class _AllOf:
    members: Callable[[], Sequence[Any]]


def tag(wire: str, aliases: Mapping[str, str] | None = None) -> TagReader:
    """The discriminator of a tagged union: the value of the ``wire`` property
    of a mapping, or of the field with that wire name of a model. ``aliases``
    maps the other tag values of the discriminator mapping to their variant's
    tag (``{"puppy": "dog"}`` when both select ``Dog``)."""
    return TagReader(wire, tuple((aliases or {}).items()))


def pattern(source: str) -> _Pattern:
    """A JSON Schema ``pattern`` (searched, not anchored). A pattern Python's
    ``re`` cannot compile is not enforced, so importing the SDK never fails;
    the server still validates it."""
    return _Pattern(source)


def string_format(name: str) -> _Format:
    """A JSON Schema string ``format`` the SDK checks (``uuid`` as any
    8-4-4-4-12 hex form, ``email``, ``date-time`` with a ``Z`` or numeric
    offset, ``date``, ``ipv4``, ``ipv6``), with the same rules as the
    TypeScript SDK, so both accept the same values."""
    return _Format(name)


def exact(values: Sequence[object]) -> _Exact:
    """Checks a ``Literal`` of booleans or integers by JSON equality:
    ``True`` is not ``1`` and ``1`` is not ``True``, ``1.0`` is ``1``."""
    return _Exact(tuple(values))


def one_of(values: Sequence[object]) -> _OneOf:
    """Admits exactly the JSON values ``values`` (an ``enum`` or ``const``
    with members a ``Literal`` cannot express)."""
    return _OneOf(tuple(values))


def all_of(members: Callable[[], Sequence[Any]]) -> _AllOf:
    """Admits a value every member type admits (an ``allOf`` that could not
    be merged into one model); the value is kept as given."""
    return _AllOf(members)


# ---------------------------------------------------------------- types

_BAD: Any = object()


def _is_file(value: object) -> bool:
    """A binary value the runtime reads itself: a bytearray or memoryview, a
    binary file object, or a ``(filename, content[, content_type])`` tuple."""
    if isinstance(value, bytearray | memoryview):
        return True
    if isinstance(value, tuple):
        parts = cast("tuple[object, ...]", value)
        return len(parts) in (2, 3) and (parts[0] is None or isinstance(parts[0], str))
    return callable(getattr(value, "read", None))


def _same(a: object, b: object) -> bool:
    """JSON equality: ``True`` is not ``1``, ``1`` is ``1.0``."""
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if isinstance(a, int | float) and isinstance(b, int | float):
        return a == b
    if isinstance(a, Mapping) and isinstance(b, Mapping):
        ma = cast("Mapping[object, object]", a)
        mb = cast("Mapping[object, object]", b)
        return ma.keys() == mb.keys() and all(_same(ma[k], mb[k]) for k in ma)
    if isinstance(a, list | tuple) and isinstance(b, list | tuple):
        sa = cast("Sequence[object]", a)
        sb = cast("Sequence[object]", b)
        return len(sa) == len(sb) and all(_same(x, y) for x, y in zip(sa, sb, strict=True))
    return type(cast("object", a)) is type(b) and a == b


type Bytes = bytes
"""Bytes; a base64 string in JSON. As a request argument it takes bytes; a
file object or a ``(filename, content[, content_type])`` tuple only in a
multipart body, where the runtime reads it."""

type Binary = bytes
"""A whole binary request body: bytes, or a file object or ``(filename,
content[, content_type])`` tuple the runtime reads."""

type Float = float
"""A number: finite, as in JSON (``nan`` and infinities are not numbers
there, and the TypeScript SDK rejects them)."""

type Int = int
"""An integer. JSON Schema's ``integer`` is any number without a fractional
part, so ``1.0`` is the integer ``1``; ``1.5``, ``"1"`` and ``True`` are not
integers."""

type Int32 = int
"""A 32-bit integer (see ``Int``)."""

type Never = Any
"""A schema no value satisfies."""

# Aliases of the scalar kinds above are told apart by identity.
_KINDS: dict[TypeAliasType, str] = {
    Bytes: "bytes",
    Binary: "binary",
    Float: "float",
    Int: "int",
    Int32: "int32",
    Never: "never",
}


def union(*members: Any) -> Any:
    """The union of ``members`` as a runtime value (a validator's type): type
    checkers reject ``|`` between ``Annotated[...]`` forms outside
    annotations."""
    result: Any = members[0]
    for member in members[1:]:
        result = result | member
    return result


def field(
    *,
    default: Any = dataclasses.MISSING,
    default_factory: Any = dataclasses.MISSING,
    wire: str | None = None,
    repr: bool = True,
) -> Any:
    """A model field with its wire name (when it differs from the attribute),
    its default (``UNSET`` for a field that may be left out) and whether it
    shows in ``repr()`` (a sensitive field does not)."""
    return dataclasses.field(
        default=default,
        default_factory=default_factory,
        repr=repr,
        metadata={"wire": wire} if wire is not None else {},
    )


@dataclass_transform(kw_only_default=True, field_specifiers=(field,))
def model[T](
    *, extra: Literal["forbid", "allow"] | Callable[[], Any] = "forbid"
) -> Callable[[type[T]], type[T]]:
    """Makes a class a model: a ``kw_only`` dataclass whose members are
    validated by ``Request`` and ``Response``. ``extra`` says what a member
    that is no field does: ``"forbid"`` (the default) refuses it, ``"allow"``
    keeps it in ``model_extra``, and a callable returning a type keeps it
    after validating it as that type."""

    def decorate(cls: type[T]) -> type[T]:
        result = dataclasses.dataclass(kw_only=True)(cls)
        setattr(result, _EXTRA_POLICY, extra)
        return result

    return decorate


class Model:
    """Base of the generated models."""

    @classmethod
    def model_validate(cls, value: object) -> Self:
        """The model built from a decoded JSON value (wire names, or the
        attribute names); raises ``ValidationError``."""
        issues: list[Issue] = []
        result = _model_node(cls).check(value, [], "response", issues)
        if issues:
            raise ValidationError(issues)
        return cast("Self", result)

    def model_dump(
        self, *, mode: Literal["python", "json"] = "python", by_alias: bool = True
    ) -> dict[str, Any]:
        """The model as a mapping: by wire name (``by_alias``) or attribute
        name, without the fields left ``UNSET``; ``json`` makes it JSON-ready
        (bytes as base64)."""
        return cast("dict[str, Any]", dump(self, json=mode == "json", by_alias=by_alias))


def validate(tp: Any, value: object) -> Any:
    """``value`` as a ``tp`` (any annotation of a generated module); raises
    ``ValidationError``."""
    issues: list[Issue] = []
    result = _compile(tp).check(value, [], "response", issues)
    if issues:
        raise ValidationError(issues)
    return result


def dump(value: object, *, json: bool = False, by_alias: bool = True) -> object:
    """``value`` as plain data: models as mappings (by wire name when
    ``by_alias``, without the fields left ``UNSET``), lists as lists; with
    ``json`` bytes become base64 strings."""
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        out: dict[str, object] = {}
        for f in dataclasses.fields(value):
            item = getattr(value, f.name)
            if f.name == EXTRA_ATTRIBUTE:
                for key, extra in cast("Mapping[str, object]", item).items():
                    out[key] = dump(extra, json=json, by_alias=by_alias)
            elif item is not UNSET:
                name = cast("str", f.metadata.get("wire", f.name)) if by_alias else f.name
                out[name] = dump(item, json=json, by_alias=by_alias)
        return out
    if isinstance(value, Mapping):
        return {
            k: dump(v, json=json, by_alias=by_alias) for k, v in cast("Mapping[Any, object]", value).items()
        }
    if isinstance(value, list | tuple):
        return [dump(v, json=json, by_alias=by_alias) for v in cast("Sequence[object]", value)]
    if json and isinstance(value, bytes | bytearray | memoryview):
        return base64.b64encode(bytes(cast("bytes", value))).decode("ascii")
    return value


# ---------------------------------------------------------------- nodes


def _fail(issues: list[Issue], path: Path, message: str, code: str) -> Any:
    issues.append({"path": list(path), "message": message, "code": code})
    return _BAD


class _Node:
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        """The value as this type reads it, or ``_BAD`` with the problems
        appended to ``issues``."""
        raise NotImplementedError


class _AnyNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        return value if mode == "response" else dump(value, json=mode == "json")


class _NoneNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if value is None:
            return None
        return _fail(issues, path, "Input should be None", "none_required")


class _StrNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if isinstance(value, str):
            return value
        return _fail(issues, path, "Input should be a valid string", "string_type")


class _BoolNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if isinstance(value, bool):
            return value
        return _fail(issues, path, "Input should be a valid boolean", "bool_type")


class _IntNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if isinstance(value, bool):
            return _fail(issues, path, "Input should be a valid integer", "int_type")
        if isinstance(value, float):
            if not math.isfinite(value) or not value.is_integer():
                return _fail(
                    issues,
                    path,
                    "Input should be a valid integer, got a number with a fractional part",
                    "int_from_float",
                )
            return int(value)
        if isinstance(value, int):
            return value
        return _fail(issues, path, "Input should be a valid integer", "int_type")


class _FloatNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if isinstance(value, bool) or not isinstance(value, int | float):
            return _fail(issues, path, "Input should be a valid number", "float_type")
        if isinstance(value, float) and not math.isfinite(value):
            return _fail(issues, path, "Input should be a finite number", "finite_number")
        return value


def _decode_base64(value: str) -> bytes:
    try:
        return base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("expected a base64 string") from error


class _BytesNode(_Node):
    """A bytes field. As a request argument (``json``, ``plain``) it takes
    bytes, as the TypeScript SDK takes a ``Uint8Array``, plus files in a
    multipart body; elsewhere (a response, a model built by hand) a string is
    base64 as in JSON, and a file is kept for a multipart body."""

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if isinstance(value, str):
            if mode != "response":
                return _fail(issues, path, "Input should be bytes, not text", "bytes_type")
            try:
                return _decode_base64(value)
            except ValueError:
                return _fail(
                    issues, path, "Input should be a valid bytes: expected a base64 string", "bytes_parsing"
                )
        if mode == "json":
            if isinstance(value, bytearray | memoryview):
                return base64.b64encode(bytes(cast("bytes", value))).decode("ascii")
            if _is_file(value):
                return _fail(
                    issues,
                    path,
                    "Input should be bytes: a file is sent only in a multipart body",
                    "bytes_type",
                )
        elif _is_file(value):
            return value
        if isinstance(value, bytes):
            return base64.b64encode(value).decode("ascii") if mode == "json" else value
        return _fail(issues, path, "Input should be a valid bytes", "bytes_type")


class _BinaryNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if _is_file(value) or isinstance(value, bytes):
            return value
        return _fail(issues, path, "Input should be a valid bytes", "bytes_type")


class _NeverNode(_Node):
    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        return _fail(issues, path, "Value error, no value is valid here", "value_error")


def _quoted(values: Sequence[object]) -> str:
    texts = [repr(v) for v in values]
    return texts[0] if len(texts) == 1 else f"{', '.join(texts[:-1])} or {texts[-1]}"


class _LiteralNode(_Node):
    def __init__(self, values: tuple[object, ...]) -> None:
        self.values = values

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if any(_same(value, v) for v in self.values):
            return value
        return _fail(issues, path, f"Input should be {_quoted(self.values)}", "literal_error")


class _ListNode(_Node):
    def __init__(self, item: _Node) -> None:
        self.item = item

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        # A request takes any sequence (the hints say so); a response, a list.
        if not isinstance(value, list | tuple) or (mode == "response" and not isinstance(value, list)):
            return _fail(issues, path, "Input should be a valid list", "list_type")
        out: list[Any] = []
        bad = False
        for i, item in enumerate(cast("Sequence[object]", value)):
            result = self.item.check(item, [*path, i], mode, issues)
            bad = bad or result is _BAD
            out.append(result)
        return _BAD if bad else out


class _DictNode(_Node):
    def __init__(self, item: _Node) -> None:
        self.item = item

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if not isinstance(value, Mapping):
            return _fail(issues, path, "Input should be a valid dictionary", "dict_type")
        out: dict[str, Any] = {}
        bad = False
        for key, item in cast("Mapping[object, object]", value).items():
            if not isinstance(key, str):
                bad = True
                _fail(issues, [*path, str(key)], "Keys should be strings", "dict_key_type")
                continue
            result = self.item.check(item, [*path, key], mode, issues)
            bad = bad or result is _BAD
            out[key] = result
        return _BAD if bad else out


def _depth(found: list[Issue]) -> int:
    return max((len(i["path"]) for i in found), default=0)


class _UnionNode(_Node):
    """Left to right: the first member that admits the value. When none does,
    the problems of the member that got furthest into the value are reported
    (members that only admit ``None`` or ``UNSET`` are left out)."""

    def __init__(self, members: list[_Node]) -> None:
        self.members = members

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        best: list[Issue] | None = None
        first: list[Issue] | None = None
        for member in self.members:
            found: list[Issue] = []
            result = member.check(value, path, mode, found)
            if result is not _BAD:
                return result
            first = first if first is not None else found
            if isinstance(member, _NoneNode) or (
                isinstance(member, _LiteralNode) and member.values == (UNSET,)
            ):
                continue
            if best is None or _depth(found) > _depth(best):
                best = found
        issues.extend(best if best is not None else first or [])
        return _BAD


class _TaggedNode(_Node):
    def __init__(self, members: dict[str, _Node], reader: TagReader, attrs: tuple[str, ...] = ()) -> None:
        self.members = members
        self.reader = reader
        # The attribute names of the variants' tag field: a mapping by
        # attribute names (a ``Model.Input``) carries the tag under one of them.
        self.attrs = attrs

    def _read(self, value: object) -> object:
        wire = self.reader.wire
        if isinstance(value, Mapping):
            given = cast("Mapping[str, object]", value)
            if wire in given:
                return given[wire]
            return next((given[a] for a in self.attrs if a in given), None)
        if dataclasses.is_dataclass(value) and not isinstance(value, type):
            for f in dataclasses.fields(value):
                if f.metadata.get("wire", f.name) == wire:
                    return getattr(value, f.name)
            extra = cast("Mapping[str, object]", getattr(value, EXTRA_ATTRIBUTE, {}))
            return extra.get(wire)
        return None

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        found = self._read(value)
        if isinstance(found, str):
            found = dict(self.reader.aliases).get(found, found)
        if not isinstance(found, str):
            if not isinstance(value, Mapping) and not (
                dataclasses.is_dataclass(value) and not isinstance(value, type)
            ):
                return _fail(
                    issues,
                    path,
                    "Input should be a valid dictionary or object to extract tag",
                    "model_attributes_type",
                )
            return _fail(
                issues,
                path,
                f"Unable to extract tag using discriminator '{self.reader.wire}'",
                "union_tag_not_found",
            )
        member = self.members.get(found)
        if member is None:
            expected = _quoted(list(self.members))
            return _fail(
                issues,
                path,
                f"Input tag '{found}' found using '{self.reader.wire}' does not match any of the expected tags: {expected}",
                "union_tag_invalid",
            )
        return member.check(value, path, mode, issues)


class _AliasNode(_Node):
    """A ``type`` alias, read on first use (aliases may refer to each other
    and to themselves)."""

    def __init__(self, alias: TypeAliasType) -> None:
        self.alias = alias
        self.node: _Node | None = None

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if self.node is None:
            self.node = _compile(self.alias.__value__)
        return self.node.check(value, path, mode, issues)


class _AllOfNode(_Node):
    def __init__(self, members: Callable[[], Sequence[Any]]) -> None:
        self.members = members
        self.nodes: list[_Node] | None = None

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        if self.nodes is None:
            self.nodes = [_compile(m) for m in self.members()]
        bad = False
        for node in self.nodes:
            bad = node.check(value, path, "response", issues) is _BAD or bad
        if bad:
            return _BAD
        return value if mode == "response" else dump(value, json=mode == "json")


_DATE = (
    r"(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00|[13579][26]00)-02-29"
    r"|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)"
    r"|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))"
)
_IPV4_PART = r"(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])"
_H16 = r"[0-9a-fA-F]{1,4}"
_FORMATS: dict[str, re.Pattern[str]] = {
    "uuid": re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"),
    "email": re.compile(r"[^\s@\"]{1,64}@[^\s@]{1,255}"),
    "date-time": re.compile(
        _DATE
        + r"T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\.[0-9]+)?(?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])"
    ),
    "date": re.compile(_DATE),
    "ipv4": re.compile(rf"(?:{_IPV4_PART}\.){{3}}{_IPV4_PART}"),
    "ipv6": re.compile(
        rf"(?:(?:{_H16}:){{7}}{_H16}|(?:{_H16}:){{1,7}}:|(?:{_H16}:){{1,6}}:{_H16}|(?:{_H16}:){{1,5}}(?::{_H16}){{1,2}}"
        rf"|(?:{_H16}:){{1,4}}(?::{_H16}){{1,3}}|(?:{_H16}:){{1,3}}(?::{_H16}){{1,4}}|(?:{_H16}:){{1,2}}(?::{_H16}){{1,5}}"
        rf"|{_H16}:(?:(?::{_H16}){{1,6}})|:(?:(?::{_H16}){{1,7}}|:))"
    ),
}


def _compile_pattern(source: str) -> re.Pattern[str] | None:
    try:
        return re.compile(source)
    except (re.error, OverflowError, RecursionError):
        return None


def _limit_issue(limits: Limits, value: object) -> tuple[str, str] | None:
    """The first bound ``value`` breaks: (message, code)."""
    if isinstance(value, str | list):
        size = len(cast("str | list[object]", value))
        noun = "characters" if isinstance(value, str) else "items"
        kind = "string" if isinstance(value, str) else "list"
        if limits.min_length is not None and size < limits.min_length:
            return (
                f"{kind.capitalize()} should have at least {limits.min_length} {noun}",
                f"{kind}_too_short",
            )
        if limits.max_length is not None and size > limits.max_length:
            return (f"{kind.capitalize()} should have at most {limits.max_length} {noun}", f"{kind}_too_long")
        return None
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    if limits.ge is not None and not value >= limits.ge:
        return (f"Input should be greater than or equal to {limits.ge}", "greater_than_equal")
    if limits.le is not None and not value <= limits.le:
        return (f"Input should be less than or equal to {limits.le}", "less_than_equal")
    if limits.gt is not None and not value > limits.gt:
        return (f"Input should be greater than {limits.gt}", "greater_than")
    if limits.lt is not None and not value < limits.lt:
        return (f"Input should be less than {limits.lt}", "less_than")
    if limits.multiple_of is not None:
        step = limits.multiple_of
        ok = value % step == 0 if isinstance(value, int) and isinstance(step, int) else _multiple(value, step)
        if not ok:
            return (f"Input should be a multiple of {step}", "multiple_of")
    return None


def _multiple(value: float, step: float) -> bool:
    if step == 0:
        return False
    quotient = value / step
    return math.isfinite(quotient) and math.isclose(quotient, round(quotient), rel_tol=0, abs_tol=1e-9)


class _CheckedNode(_Node):
    """A node with the constraints of its ``Annotated`` metadata: ``exact``
    before it reads the value, bounds, ``pattern``, ``string_format`` and
    ``one_of`` on what it read."""

    def __init__(self, inner: _Node, meta: Sequence[object]) -> None:
        self.inner = inner
        self.exact = [m for m in meta if isinstance(m, _Exact)]
        self.limits = [m for m in meta if isinstance(m, Limits)]
        self.patterns = [(m.source, _compile_pattern(m.source)) for m in meta if isinstance(m, _Pattern)]
        self.formats = [m for m in meta if isinstance(m, _Format)]
        self.one_of = [m for m in meta if isinstance(m, _OneOf)]

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        for e in self.exact:
            if isinstance(value, bool | int | float) and not any(_same(value, v) for v in e.values):
                return _fail(
                    issues, path, f"Input should be {' or '.join(map(repr, e.values))}", "literal_error"
                )
        result = self.inner.check(value, path, mode, issues)
        if result is _BAD:
            return _BAD
        # Bounds apply to the value read (an integral float is an integer).
        subject = result
        for limits in self.limits:
            problem = _limit_issue(limits, subject)
            if problem is not None:
                return _fail(issues, path, *problem)
        if isinstance(subject, str):
            for source, compiled in self.patterns:
                if compiled is not None and compiled.search(subject) is None:
                    return _fail(
                        issues, path, f"String should match pattern /{source}/", "string_pattern_mismatch"
                    )
            for f in self.formats:
                if _FORMATS[f.name].fullmatch(subject) is None:
                    return _fail(issues, path, f"String should be a valid {f.name}", "string_format")
        for o in self.one_of:
            if not any(_same(value, v) for v in o.values):
                return _fail(issues, path, f"Value error, expected one of {list(o.values)!r}", "value_error")
        return result


@dataclasses.dataclass(frozen=True, slots=True)
class _ModelField:
    attr: str
    wire: str
    required: bool
    node: _Node


class _ModelNode(_Node):
    def __init__(self, cls: type) -> None:
        self.cls = cls
        self.fields: list[_ModelField] | None = None
        self.extra: Literal["forbid", "allow"] | _Node = "forbid"

    def _prepare(self) -> list[_ModelField]:
        cls = self.cls
        hints = get_type_hints(cls, include_extras=True)
        fields: list[_ModelField] = []
        for f in dataclasses.fields(cls):
            if f.name == EXTRA_ATTRIBUTE:
                continue
            required = f.default is dataclasses.MISSING and f.default_factory is dataclasses.MISSING
            wire = cast("str", f.metadata.get("wire", f.name))
            fields.append(_ModelField(f.name, wire, required, _compile(hints[f.name])))
        policy = getattr(cls, _EXTRA_POLICY, "forbid")
        if callable(policy):
            self.extra = _compile(cast("Callable[[], Any]", policy)())
        else:
            self.extra = cast("Literal['forbid', 'allow']", policy)
        self.fields = fields
        return fields

    def check(self, value: object, path: Path, mode: Mode, issues: list[Issue]) -> Any:
        fields = self.fields if self.fields is not None else self._prepare()
        cls = self.cls
        source: Mapping[object, object]
        if isinstance(value, cls):
            if mode == "response":
                return value
            source = {f.attr: getattr(value, f.attr) for f in fields}
            source = {**cast("Mapping[str, object]", getattr(value, EXTRA_ATTRIBUTE, {})), **source}
        elif isinstance(value, Mapping):
            source = cast("Mapping[object, object]", value)
        else:
            return _fail(
                issues,
                path,
                f"Input should be a valid dictionary or instance of {cls.__name__}",
                "model_type",
            )
        known: set[object] = set()
        wire_out: dict[str, Any] = {}
        kwargs: dict[str, Any] = {}
        bad = False
        for f in fields:
            known.add(f.wire)
            known.add(f.attr)
            raw: object = UNSET
            if f.wire in source:
                raw = source[f.wire]
            elif f.attr in source:
                raw = source[f.attr]
            if raw is UNSET:
                if f.required:
                    bad = True
                    _fail(issues, [*path, f.wire], "Field required", "missing")
                continue
            result = f.node.check(raw, [*path, f.wire], mode, issues)
            if result is _BAD:
                bad = True
            elif mode == "response":
                kwargs[f.attr] = result
            else:
                wire_out[f.wire] = result
        extras: dict[str, Any] = {}
        for key, raw in source.items():
            if key in known:
                continue
            if not isinstance(key, str):
                bad = True
                _fail(issues, [*path, str(key)], "Keys should be strings", "dict_key_type")
            elif self.extra == "forbid":
                bad = True
                _fail(issues, [*path, key], "Extra inputs are not permitted", "extra_forbidden")
            elif isinstance(self.extra, _Node):
                result = self.extra.check(raw, [*path, key], mode, issues)
                bad = bad or result is _BAD
                extras[key] = result
            else:
                extras[key] = raw if mode == "response" else dump(raw, json=mode == "json")
        if bad:
            return _BAD
        if mode == "response":
            if hasattr(cls, _EXTRA_POLICY) and extras:
                kwargs[EXTRA_ATTRIBUTE] = extras
            return cls(**kwargs)
        wire_out.update(extras)
        return wire_out


_MODELS: dict[type, _ModelNode] = {}
_ALIASES: dict[TypeAliasType, _AliasNode] = {}


def _model_node(cls: type) -> _ModelNode:
    node = _MODELS.get(cls)
    if node is None:
        node = _MODELS[cls] = _ModelNode(cls)
    return node


def _compile(tp: Any) -> _Node:
    """The node that validates ``tp``, an annotation of a generated module."""
    if tp is Any:
        return _AnyNode()
    if tp is None or tp is NoneType:
        return _NoneNode()
    if isinstance(tp, TypeAliasType):
        kind = _KINDS.get(tp)
        if kind is not None:
            return _scalar(kind)
        node = _ALIASES.get(tp)
        if node is None:
            node = _ALIASES[tp] = _AliasNode(tp)
        return node
    origin = get_origin(tp)
    if origin is Annotated:
        base, *meta = get_args(tp)
        return _annotated(base, meta)
    if origin is Literal:
        return _LiteralNode(get_args(tp))
    if origin is Union or origin is UnionType:
        return _UnionNode([_compile(m) for m in get_args(tp)])
    if origin is list:
        return _ListNode(_compile(get_args(tp)[0]))
    if origin is dict:
        return _DictNode(_compile(get_args(tp)[1]))
    if tp is str:
        return _StrNode()
    if tp is bool:
        return _BoolNode()
    if tp is int:
        return _IntNode()
    if tp is float:
        return _FloatNode()
    if tp is bytes:
        return _BytesNode()
    if isinstance(tp, type) and dataclasses.is_dataclass(tp):
        return _model_node(tp)
    raise TypeError(f"no validation for the annotation {tp!r}")


def _scalar(kind: str) -> _Node:
    if kind == "bytes":
        return _BytesNode()
    if kind == "binary":
        return _BinaryNode()
    if kind == "float":
        return _FloatNode()
    if kind == "never":
        return _NeverNode()
    if kind == "int32":
        return _CheckedNode(_IntNode(), [Limits(ge=-(2**31), le=2**31 - 1)])
    return _IntNode()


def _tag_attributes(union: Any, wire: str) -> tuple[str, ...]:
    """The attribute names under which the model variants of ``union`` hold
    the field with the wire name ``wire`` (other than ``wire`` itself)."""
    names: dict[str, None] = {}
    for member in get_args(union):
        cls = get_args(member)[0] if get_origin(member) is Annotated else member
        if isinstance(cls, type) and dataclasses.is_dataclass(cls):
            for f in dataclasses.fields(cls):
                if f.metadata.get("wire", f.name) == wire and f.name != wire:
                    names[f.name] = None
    return tuple(names)


def _annotated(base: Any, meta: list[object]) -> _Node:
    for m in meta:
        if isinstance(m, _AllOf):
            return _AllOfNode(m.members)
    readers = [m for m in meta if isinstance(m, TagReader)]
    if readers and (get_origin(base) is Union or get_origin(base) is UnionType):
        members: dict[str, _Node] = {}
        for member in get_args(base):
            tags = (
                [m for m in get_args(member)[1:] if isinstance(m, Tag)]
                if get_origin(member) is Annotated
                else []
            )
            if tags:
                members.setdefault(tags[0].value, _compile(member))
        return _TaggedNode(members, readers[0], _tag_attributes(base, readers[0].wire))
    node = _compile(base)
    if any(isinstance(m, Limits | _Pattern | _Format | _Exact | _OneOf) for m in meta):
        return _CheckedNode(node, meta)
    return node


# ------------------------------------------------------------ validators


@dataclasses.dataclass(frozen=True, slots=True)
class Arg:
    """One argument of an operation: its type, whether it is required, and
    whether its value is dumped JSON-ready (``False`` for bytes, text, form
    and multipart bodies, which the runtime encodes itself)."""

    type: Any
    required: bool
    json: bool = True


class Request:
    """Validates the arguments of an operation (a mapping by argument name)
    strictly (no coercion between JSON types) and normalizes them: models
    become mappings by wire name, ``UNSET`` is removed."""

    def __init__(self, args: Callable[[], Mapping[str, Arg]]) -> None:
        self._args = args
        self._nodes: dict[str, tuple[Arg, _Node]] | None = None

    def _ready(self) -> dict[str, tuple[Arg, _Node]]:
        if self._nodes is None:
            self._nodes = {k: (a, _compile(a.type)) for k, a in self._args().items()}
        return self._nodes

    def validate(self, value: object) -> Valid[dict[str, Any]] | Invalid:
        if not isinstance(value, Mapping):
            return Invalid([{"path": [], "message": "expected a mapping of arguments by name"}])
        given = cast("Mapping[object, object]", value)
        nodes = self._ready()
        issues: list[Issue] = []
        data: dict[str, Any] = {}
        for name, (arg, node) in nodes.items():
            raw = given.get(name, UNSET)
            if raw is UNSET:
                if arg.required:
                    issues.append({"path": [name], "message": "Field required", "code": "missing"})
                continue
            result = node.check(raw, [name], "json" if arg.json else "plain", issues)
            if result is not _BAD:
                data[name] = result
        for key, raw in given.items():
            if key not in nodes and raw is not UNSET:
                expected = ", ".join(nodes) or "no arguments"
                issues.append(
                    {
                        "path": [str(key)],
                        "message": f"Unexpected argument; expected one of: {expected}",
                        "code": "extra_forbidden",
                    }
                )
        return Invalid(issues) if issues else Valid(data)


class Response:
    """Validates a decoded success body into its typed value (models)."""

    def __init__(self, type_: Callable[[], Any]) -> None:
        self._type = type_
        self._node: _Node | None = None

    def validate(self, value: object) -> Valid[Any] | Invalid:
        if self._node is None:
            self._node = _compile(self._type())
        issues: list[Issue] = []
        result = self._node.check(value, [], "response", issues)
        return Invalid(issues) if issues else Valid(result)
