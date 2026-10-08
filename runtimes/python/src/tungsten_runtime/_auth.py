# SPDX-License-Identifier: Apache-2.0
"""Auth resolution (planning/06 "Auth profiles", ``runtimes/ts/src/auth.ts``):
an operation's security is an OR of AND-sets of scheme names; the first
alternative that ``ClientOptions.auth`` fully satisfies is applied. A
composite profile satisfies the scheme names in its ``satisfies`` list when
every part it needs for this request is configured.

Credentials per scheme name: a string secret for ``api_key``,
``http_bearer`` and ``http_basic`` (``user:password``); for ``oauth2`` an
access token, or ``{"client_id": ..., "client_secret": ...}`` to fetch one
from the token URL (client credentials, cached, single-flight); for a
composite profile a mapping keyed by cookie name or config key (``bearer``
for its bearer part).
"""

from __future__ import annotations

from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field

from ._effects import Flow
from ._json import js_string
from ._util import b64, is_array, is_record, str_field, utf8
from .types import HttpMethod

SAFE_METHODS = frozenset({"GET", "HEAD", "OPTIONS", "TRACE"})


def is_safe_method(method: str) -> bool:
    return method in SAFE_METHODS


@dataclass(frozen=True, slots=True)
class AuthHeader:
    name: str
    value: str
    secret: bool


@dataclass(slots=True)
class AuthPlan:
    """Credentials to apply to one request."""

    headers: list[AuthHeader] = field(default_factory=list[AuthHeader])
    cookies: list[tuple[str, str]] = field(default_factory=list[tuple[str, str]])
    query: list[tuple[str, str]] = field(default_factory=list[tuple[str, str]])


@dataclass(frozen=True, slots=True)
class AuthOk:
    plan: AuthPlan
    secrets: list[str]


@dataclass(frozen=True, slots=True)
class AuthMissing:
    remediation: str


class TokenError(Exception):
    """Raised by a token source; the message is shown in the AUTH_FAILED envelope."""


@dataclass(frozen=True, slots=True)
class OAuth2Client:
    """What a token source needs to fetch an OAuth2 client-credentials token."""

    scheme: str
    token_url: str
    scopes: list[str]
    client_id: str
    client_secret: str


#: Fetches (and caches) OAuth2 client-credentials tokens.
type TokenSource = Callable[[OAuth2Client], Flow[str]]

type _Apply = Callable[[AuthPlan, list[str], TokenSource], Flow[str | None]]


@dataclass(frozen=True, slots=True)
class _Satisfier:
    key: str
    apply: _Apply


@dataclass(frozen=True, slots=True)
class _Missing:
    missing: list[str]


def _records(value: object) -> list[Mapping[str, object]]:
    return [item for item in value if is_record(item)] if is_array(value) else []


def _composite_key(part: Mapping[str, object]) -> str:
    kind = part.get("kind")
    if kind == "cookie":
        return str_field(part, "name") or ""
    if kind == "bearer":
        return "bearer"
    equals_cookie = str_field(part, "equals_cookie")
    if equals_cookie is not None:
        return equals_cookie
    from_config = str_field(part, "from_config")
    return from_config if from_config is not None else str_field(part, "name") or ""


def _check_prefix(token: str, prefix: object, what: str) -> str | None:
    if isinstance(prefix, str) and prefix != "" and not token.startswith(prefix):
        return f'The {what} credential must start with "{prefix}"; check that the right kind of token is configured.'
    return None


def _try_composite(c: Mapping[str, object], config: object, method: HttpMethod) -> _Satisfier | _Missing:
    name = str_field(c, "name") or ""
    if not is_record(config):
        return _Missing([f"auth.{name} (a mapping for the composite profile {name})"])
    values = config
    mutation = not is_safe_method(method)
    parts = [
        p
        for p in _records(c.get("parts"))
        if not (p.get("kind") == "header" and p.get("mutation_only") is True and not mutation)
    ]
    missing: list[str] = []
    for part in parts:
        key = _composite_key(part)
        value = values.get(key)
        if not isinstance(value, str) or value == "":
            kind = part.get("kind")
            part_name = str_field(part, "name") or ""
            if kind == "cookie":
                role = f"cookie {part_name}"
            elif kind == "bearer":
                role = "bearer token"
            else:
                equals = str_field(part, "equals_cookie")
                role = f"header {part_name}{f' (equal to cookie {equals})' if equals else ''}"
            missing.append(f"auth.{name}[{js_string(key)}] ({role} of the composite profile {name})")
    if len(missing) > 0:
        return _Missing(missing)

    def apply(plan: AuthPlan, secrets: list[str], tokens: TokenSource) -> Flow[str | None]:
        del tokens
        yield from ()
        for part in parts:
            value = str(values.get(_composite_key(part)))
            kind = part.get("kind")
            if kind == "cookie":
                plan.cookies.append((str_field(part, "name") or "", value))
                secrets.append(value)
            elif kind == "bearer":
                problem = _check_prefix(value, part.get("prefix"), f"{name} bearer")
                if problem is not None:
                    return problem
                plan.headers.append(AuthHeader("Authorization", f"Bearer {value}", True))
                secrets.append(value)
            else:
                secret = part.get("from_config") is None
                plan.headers.append(AuthHeader(str_field(part, "name") or "", value, secret))
                if secret:
                    secrets.append(value)
        return None

    return _Satisfier(f"composite:{name}", apply)


def _try_direct(scheme: Mapping[str, object], config: object) -> _Satisfier | _Missing:
    name = str_field(scheme, "name") or ""
    kind = scheme.get("kind")
    if kind == "oauth2" and is_record(config):
        client_id = config.get("client_id")
        client_secret = config.get("client_secret")
        token_url = scheme.get("token_url")
        if (
            isinstance(client_id, str)
            and isinstance(client_secret, str)
            and isinstance(token_url, str)
            and token_url
        ):
            scopes_value = scheme.get("scopes")
            scopes = [s for s in scopes_value if isinstance(s, str)] if is_array(scopes_value) else []
            client = OAuth2Client(name, token_url, scopes, client_id, client_secret)

            def apply_oauth(plan: AuthPlan, secrets: list[str], tokens: TokenSource) -> Flow[str | None]:
                secrets.append(client.client_secret)
                try:
                    token = yield from tokens(client)
                except TokenError as error:
                    return str(error)
                except Exception:
                    return f"Could not obtain an OAuth2 token for {name}."
                secrets.append(token)
                plan.headers.append(AuthHeader("Authorization", f"Bearer {token}", True))
                return None

            return _Satisfier(f"scheme:{name}", apply_oauth)
        return _Missing([f"auth.{name} (an access token, or {{client_id, client_secret}} for the token URL)"])
    if not isinstance(config, str) or config == "":
        if kind == "http_basic":
            what = f'"user:password" for HTTP Basic scheme {name}'
        elif kind == "api_key":
            what = f"API key for scheme {name} ({scheme.get('location')} {scheme.get('wire')})"
        else:
            what = f"bearer token for scheme {name}"
        return _Missing([f"auth.{name} ({what})"])
    secret = config

    def apply(plan: AuthPlan, secrets: list[str], tokens: TokenSource) -> Flow[str | None]:
        del tokens
        yield from ()
        secrets.append(secret)
        if kind == "api_key":
            location = scheme.get("location")
            wire = str_field(scheme, "wire") or ""
            if location == "header":
                plan.headers.append(AuthHeader(wire, secret, True))
            elif location == "query":
                plan.query.append((wire, secret))
            else:
                plan.cookies.append((wire, secret))
            return None
        if kind == "http_basic":
            encoded = b64(utf8(secret))
            secrets.append(encoded)
            plan.headers.append(AuthHeader("Authorization", f"Basic {encoded}", True))
            return None
        problem = _check_prefix(secret, scheme.get("prefix"), name) if kind == "http_bearer" else None
        if problem is not None:
            return problem
        plan.headers.append(AuthHeader("Authorization", f"Bearer {secret}", True))
        return None

    return _Satisfier(f"scheme:{name}", apply)


def _configured(auth: Mapping[str, object], name: str) -> bool:
    return auth.get(name) is not None


def _try_scheme_name(
    schemes: Sequence[Mapping[str, object]], auth: Mapping[str, object], name: str, method: HttpMethod
) -> _Satisfier | _Missing:
    missing: list[str] = []
    for scheme in schemes:
        satisfies = scheme.get("satisfies")
        if (
            scheme.get("kind") == "composite"
            and is_array(satisfies)
            and name in satisfies
            and is_array(scheme.get("parts"))
        ):
            profile = str_field(scheme, "name") or ""
            if not _configured(auth, profile):
                missing.append(f"auth.{profile} (composite profile {profile})")
                continue
            attempt = _try_composite(scheme, auth.get(profile), method)
            if isinstance(attempt, _Satisfier):
                return attempt
            missing.extend(attempt.missing)
    direct = next((s for s in schemes if s.get("name") == name and s.get("kind") != "composite"), None)
    if direct is not None:
        attempt = _try_direct(direct, auth.get(name))
        if isinstance(attempt, _Satisfier):
            return attempt
        # When a composite profile covers the name, its missing parts explain
        # the gap better than the raw spec scheme.
        if len(missing) == 0:
            missing.extend(attempt.missing)
    elif len(missing) == 0:
        missing.append(f"a scheme named {name} (the API descriptor does not define it)")
    return _Missing(missing)


def _touched(schemes: Sequence[Mapping[str, object]], auth: Mapping[str, object], name: str) -> bool:
    """Whether ``auth`` has an entry for the scheme or a composite profile satisfying it."""
    if _configured(auth, name):
        return True
    for s in schemes:
        satisfies = s.get("satisfies")
        profile = s.get("name")
        if (
            s.get("kind") == "composite"
            and is_array(satisfies)
            and name in satisfies
            and isinstance(profile, str)
            and _configured(auth, profile)
        ):
            return True
    return False


def resolve_auth(
    api_auth: object,
    op_id: str,
    security: object,
    auth: Mapping[str, object],
    method: HttpMethod,
    tokens: TokenSource,
) -> Flow[AuthOk | AuthMissing]:
    """Resolve the credentials of one operation. A token source failure
    becomes an ``AuthMissing`` resolution."""
    alternatives = (
        [[n for n in alt if isinstance(n, str)] for alt in security if is_array(alt)]
        if is_array(security)
        else []
    )
    if len(alternatives) == 0:
        return AuthOk(AuthPlan(), [])
    schemes = _records(api_auth)
    best: tuple[list[str], bool] | None = None
    # An empty alternative (anonymous access) is the fallback, never
    # preferred over configured credentials.
    ordered = [alt for alt in alternatives if len(alt) > 0] + [alt for alt in alternatives if len(alt) == 0]
    for names in ordered:
        satisfiers: dict[str, _Satisfier] = {}
        missing: list[str] = []
        for name in names:
            attempt = _try_scheme_name(schemes, auth, name, method)
            if isinstance(attempt, _Satisfier):
                satisfiers[attempt.key] = attempt
            else:
                missing.extend(attempt.missing)
        if len(missing) == 0:
            plan = AuthPlan()
            secrets: list[str] = []
            for satisfier in satisfiers.values():
                problem = yield from satisfier.apply(plan, secrets, tokens)
                if problem is not None:
                    return AuthMissing(f"{problem} Operation {op_id} was not sent.")
            return AuthOk(plan, secrets)
        # Report the alternative the caller started to configure, then the
        # one with the fewest missing pieces.
        unique = list(dict.fromkeys(missing))
        touched = any(_touched(schemes, auth, name) for name in names)
        if best is None or (touched and not best[1]) or (touched == best[1] and len(unique) < len(best[0])):
            best = (unique, touched)
    described = " OR ".join(" AND ".join(alt) or "no credentials" for alt in alternatives)
    missing_text = "; ".join(best[0]) if best is not None else "credentials"
    return AuthMissing(
        f"Operation {op_id} needs {described}. Missing: {missing_text}. "
        "Configure it in ClientOptions.auth and call again; the request was not sent."
    )
