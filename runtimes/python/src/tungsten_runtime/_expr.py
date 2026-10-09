# SPDX-License-Identifier: Apache-2.0
"""Predicates (verify ``expect``/``terminal``, poll ``until``) and the
reference expressions used by verification argument mappings and macros
(``runtimes/ts/src/expr.ts``).

Predicate: field path -> ``{"in": [...]}`` | ``{"equals": x}`` |
``{"contains": {...}}``; any other value is compared for equality. Every
entry must hold.

Expression: JSON in which a string starting with ``$`` is a reference
resolved against a scope (``$input.a.b``, ``$<name>.a``, ``$response.x``,
``$args.y``), an object ``{"expr": "<ref> in [..]" | "<ref> == x" |
"<ref> != x"}`` evaluates to a boolean, and anything else is a literal.

A dry evaluation (macro previews) knows the input but not the results of the
steps before: a reference to such a result evaluates to a placeholder
string, ``<from step NAME: path>`` (``<from step NAME>`` for the whole
result), and so does an ``expr`` over one.
"""

from __future__ import annotations

import re
from collections.abc import Collection, Mapping

from ._json import display_json, parse_json
from ._util import contains_subset, deep_equal, get_path, is_array, is_record, split_path
from .sentinels import UNSET

type Scope = Mapping[str, object]


def evaluate_predicate(predicate: object, value: object) -> bool:
    """True when every entry of ``predicate`` holds on ``value``. Malformed
    predicates never hold."""
    if predicate is None or predicate is UNSET:
        return True
    if not is_record(predicate):
        return False
    return all(_holds(test, get_path(value, path)) for path, test in predicate.items())


def _holds(test: object, actual: object) -> bool:
    if is_record(test) and len(test) == 1:
        if "in" in test:
            candidates = test["in"]
            return is_array(candidates) and any(deep_equal(c, actual) for c in candidates)
        if "equals" in test:
            return deep_equal(test["equals"], actual)
        if "contains" in test:
            pattern = test["contains"]
            if isinstance(pattern, str):
                return (isinstance(actual, str) and pattern in actual) or (
                    is_array(actual) and any(isinstance(item, str) and item == pattern for item in actual)
                )
            if is_array(actual) and not is_array(pattern):
                return any(contains_subset(item, pattern) for item in actual)
            return contains_subset(actual, pattern)
    return deep_equal(test, actual)


def resolve_ref(ref: str, scope: Scope) -> object:
    """Resolve one ``$name.path`` reference; ``UNSET`` when the name is unbound."""
    segments = split_path(ref[1:])
    if len(segments) == 0 or segments[0] not in scope:
        return UNSET
    return get_path(scope[segments[0]], segments[1:])


def evaluate_expr(expr: object, scope: Scope, depth: int = 0) -> object:
    """Evaluate an expression. Unknown references are ``UNSET`` (dropped from
    objects); malformed ``expr`` strings are ``None``."""
    if depth > 64:
        return None
    if isinstance(expr, str):
        return resolve_ref(expr, scope) if expr.startswith("$") else expr
    if is_array(expr):
        return [evaluate_expr(item, scope, depth + 1) for item in expr]
    if is_record(expr):
        if len(expr) == 1 and isinstance(expr.get("expr"), str):
            return _evaluate_boolean(str(expr["expr"]), scope)
        out: dict[str, object] = {}
        for key, item in expr.items():
            value = evaluate_expr(item, scope, depth + 1)
            if value is not UNSET:
                out[key] = value
        return out
    return expr


def placeholder(name: str, path: str) -> str:
    """The placeholder a dry evaluation shows for a result not produced yet."""
    return f"<from step {name}>" if path == "" else f"<from step {name}: {path}>"


_PLACEHOLDER = re.compile(r"<from step [^<>]+>")


def placeholders_in(value: object, out: list[str] | None = None, depth: int = 0) -> list[str]:
    """The placeholders in ``value``, at any depth, in order of appearance."""
    found: list[str] = out if out is not None else []
    if depth > 64:
        return found
    if isinstance(value, str):
        if _PLACEHOLDER.fullmatch(value) is not None:
            found.append(value)
    elif is_array(value):
        for item in value:
            placeholders_in(item, found, depth + 1)
    elif is_record(value):
        for item in value.values():
            placeholders_in(item, found, depth + 1)
    return found


def contains_placeholder(value: object) -> bool:
    """Whether ``value`` is a placeholder, or holds one at any depth."""
    return len(placeholders_in(value)) > 0


def _pending_ref(ref: str, pending: Collection[str]) -> tuple[str, str] | None:
    segments = split_path(ref[1:])
    if len(segments) == 0:
        return None
    name = segments[0]
    if name not in pending or not ref.startswith(f"${name}"):
        return None
    rest = ref[len(name) + 1 :]
    return name, rest[1:] if rest.startswith(".") else rest


_LEADING_REF = re.compile(r"\$[^\s=!]+")


def evaluate_dry(expr: object, scope: Scope, pending: Collection[str], depth: int = 0) -> object:
    """``evaluate_expr`` for a dry run: references to the ``pending`` names
    (results of steps that have not run) become placeholders."""
    if depth > 64:
        return None
    if isinstance(expr, str):
        if not expr.startswith("$"):
            return expr
        ref = _pending_ref(expr, pending)
        return placeholder(*ref) if ref is not None else resolve_ref(expr, scope)
    if is_array(expr):
        return [evaluate_dry(item, scope, pending, depth + 1) for item in expr]
    if is_record(expr):
        if len(expr) == 1 and isinstance(expr.get("expr"), str):
            source = str(expr["expr"]).strip()
            leading = _LEADING_REF.match(source)
            ref = _pending_ref(leading.group(0) if leading is not None else "", pending)
            if ref is None:
                return _evaluate_boolean(source, scope)
            rest = source[len(ref[0]) + 1 :]
            return placeholder(ref[0], rest[1:] if rest.startswith(".") else rest)
        out: dict[str, object] = {}
        for key, item in expr.items():
            value = evaluate_dry(item, scope, pending, depth + 1)
            if value is not UNSET:
                out[key] = value
        return out
    return expr


def describe_predicate(predicate: object) -> str:
    """A predicate in words: ``state in ["delivered", "failed"]``,
    ``endpoints contains {...}``, ``a = 1``, joined with "and"."""
    if not is_record(predicate):
        return ""
    parts: list[str] = []
    for path, test in predicate.items():
        if is_record(test) and len(test) == 1:
            candidates = test.get("in", UNSET)
            if is_array(candidates):
                parts.append(f"{path} in [{', '.join(display_json(c) for c in candidates)}]")
                continue
            if "equals" in test:
                parts.append(f"{path} = {display_json(test['equals'])}")
                continue
            if "contains" in test:
                parts.append(f"{path} contains {display_json(test['contains'])}")
                continue
        parts.append(f"{path} = {display_json(test)}")
    return " and ".join(parts)


_SINGLE_QUOTED = re.compile(r"'((?:[^'\\]|\\.)*)'")


def _parse_literal(text: str) -> tuple[bool, object]:
    trimmed = text.strip()
    try:
        return True, parse_json(trimmed)
    except ValueError:
        normalized = _SINGLE_QUOTED.sub(lambda m: display_json(m.group(1).replace("\\'", "'")), trimmed)
        try:
            return True, parse_json(normalized)
        except ValueError:
            return False, None


_BOOLEAN = re.compile(r"\s*(\$[^\s=!]+)\s+(in|==|!=)\s+(.+)", re.DOTALL)


def _evaluate_boolean(source: str, scope: Scope) -> bool | None:
    match = _BOOLEAN.fullmatch(source)
    if match is None:
        return None
    ref, op, literal_text = match.group(1), match.group(2), match.group(3)
    ok, literal = _parse_literal(literal_text)
    if not ok:
        return None
    actual = resolve_ref(ref, scope)
    if op == "in":
        return any(deep_equal(v, actual) for v in literal) if is_array(literal) else None
    equal = deep_equal(None if actual is UNSET else actual, literal)
    return equal if op == "==" else not equal
