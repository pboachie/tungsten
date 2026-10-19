# SPDX-License-Identifier: Apache-2.0
"""OAuth2 authorization-code helpers (``runtimes/ts/src/oauth.ts``): PKCE
(RFC 7636, S256), the authorization URL, the code exchange, refresh with a
single flight per scheme, and the token store behind an ``authorizationCode``
scheme.

Credentials: ``ClientOptions.auth[scheme] = {"flow": "authorizationCode",
"client_id": ..., "client_secret": ..., "redirect_uri": ...}`` (the last two
optional). Tokens live in ``ClientOptions.token_store`` (memory by default),
never in the options, envelopes or logs.
"""

from __future__ import annotations

import contextlib
import hashlib
import threading
import time
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from itertools import chain
from typing import Generic, Literal, Protocol, TypeVar, cast
from urllib.parse import quote

from ._auth import INSECURE_HTTP_AUTH, TokenError, insecure_http
from ._classify import decode_body
from ._effects import Answered, AttemptRequest, Flow, invoke, send, shared
from ._envelope import diagnostic
from ._oauth_attempt import (
    MAX_ATTEMPTS,
    MAX_CALLBACK_BYTES,
    MAX_ITEMS,
    TOKEN_RESPONSE_BYTES,
    TTL_MS,
    AuthorizationAttempt,
    IssuerDefense,
    PendingAuthorization,
    callback_fields,
    capture,
    qualified_endpoint,
)
from ._util import (
    b64,
    b64url,
    get_path,
    is_array,
    is_number,
    is_record,
    items_of,
    random_bytes,
    str_field,
    usv,
    utf8,
)
from ._util import (
    field as field_of,
)
from .types import Err

T = TypeVar("T", covariant=True)

#: Tokens expiring within this many milliseconds count as expired.
EXPIRY_SKEW_MS = 30_000

_VERIFIER_BYTES = 32


@dataclass(frozen=True, slots=True)
class StoredToken:
    """What the store keeps for one scheme. Secrets: never log it."""

    access_token: str = field(repr=False)
    refresh_token: str | None = field(default=None, repr=False)
    #: Epoch milliseconds; None when the server gave no lifetime.
    expires_at: float | None = None
    token_type: str | None = None
    scope: str | None = None


class TokenStore(Protocol):
    """Holds the tokens of the authorization-code schemes, by scheme name."""

    def get(self, scheme: str) -> StoredToken | None: ...
    def set(self, scheme: str, token: StoredToken) -> None: ...
    def delete(self, scheme: str) -> None: ...
    def set_if_current(
        self, scheme: str, token: StoredToken, current: Callable[[StoredToken | None], bool]
    ) -> bool:
        """Check current stored token and predicate atomically with the final write."""
        ...

    def delete_if_current(self, scheme: str, current: Callable[[StoredToken | None], bool]) -> bool: ...


class AsyncTokenStore(Protocol):
    """Like ``TokenStore``, for ``AsyncClientCore`` (which accepts either)."""

    async def get(self, scheme: str) -> StoredToken | None: ...
    async def set(self, scheme: str, token: StoredToken) -> None: ...
    async def delete(self, scheme: str) -> None: ...
    async def set_if_current(
        self, scheme: str, token: StoredToken, current: Callable[[StoredToken | None], bool]
    ) -> bool: ...
    async def delete_if_current(self, scheme: str, current: Callable[[StoredToken | None], bool]) -> bool: ...


class MemoryTokenStore:
    """The default store: in memory, per client, safe across threads."""

    def __init__(self) -> None:
        self._tokens: dict[str, StoredToken] = {}
        self._lock = threading.Lock()

    def get(self, scheme: str) -> StoredToken | None:
        with self._lock:
            return self._tokens.get(scheme)

    def set(self, scheme: str, token: StoredToken) -> None:
        with self._lock:
            self._tokens[scheme] = token

    def delete(self, scheme: str) -> None:
        with self._lock:
            self._tokens.pop(scheme, None)

    def set_if_current(
        self, scheme: str, token: StoredToken, is_current: Callable[[StoredToken | None], bool]
    ) -> bool:
        """Evaluate the fence atomically with the final write; custom stores need the same contract."""
        with self._lock:
            if not is_current(self._tokens.get(scheme)):
                return False
            self._tokens[scheme] = token
            return True

    def delete_if_current(self, scheme: str, is_current: Callable[[StoredToken | None], bool]) -> bool:
        with self._lock:
            if not is_current(self._tokens.get(scheme)):
                return False
            self._tokens.pop(scheme, None)
            return True


@dataclass(frozen=True, slots=True)
class Pkce:
    """A PKCE pair (RFC 7636). The verifier is a secret until the exchange."""

    verifier: str = field(repr=False)
    challenge: str
    method: Literal["S256"] = "S256"


@dataclass(frozen=True, slots=True)
class TokenInfo:
    """What ``exchange_code`` and ``refresh`` report: no secrets."""

    scheme: str
    #: Epoch milliseconds, or None when the server gave no lifetime.
    expires_at: float | None
    scope: str | None
    #: Whether a refresh token is stored.
    refreshable: bool


@dataclass(frozen=True, slots=True)
class OAuthOk(Generic[T]):
    value: T
    ok: Literal[True] = True


OAuthResult = OAuthOk[T] | Err


def pkce_challenge(verifier: str) -> str:
    """The S256 code challenge of ``verifier``: base64url(SHA-256(ASCII(verifier)))."""
    return b64url(hashlib.sha256(utf8(verifier)).digest())


def generate_pkce() -> Pkce:
    """A fresh verifier (43 URL-safe characters from 32 random bytes) and its S256 challenge."""
    verifier = b64url(random_bytes(_VERIFIER_BYTES))
    return Pkce(verifier=verifier, challenge=pkce_challenge(verifier))


def percent_encode(text: str) -> str:
    """RFC 3986 percent-encoding: everything but ``A-Za-z0-9-._~``."""
    return quote(usv(text), safe="")


def form_body(fields: Sequence[tuple[str, str]]) -> str:
    return "&".join(f"{percent_encode(k)}={percent_encode(v)}" for k, v in fields)


def build_authorization_url(
    flow: Mapping[str, object],
    client_id: str,
    redirect_uri: str,
    *,
    scopes: Sequence[str] | None = None,
    state: str | None = None,
    code_challenge: str | None = None,
    extra: Mapping[str, str] | None = None,
) -> str:
    """The URL that sends the user to the authorization server. Query order:
    response_type, client_id, redirect_uri, scope, state, code_challenge,
    code_challenge_method, then ``extra`` in key order."""
    fields: list[tuple[str, str]] = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
    ]
    flow_scopes = flow.get("scopes")
    chosen = (
        list(scopes)
        if scopes is not None
        else [s for s in flow_scopes if isinstance(s, str)]
        if is_array(flow_scopes)
        else []
    )
    if len(chosen) > 0:
        fields.append(("scope", " ".join(chosen)))
    if state:
        fields.append(("state", state))
    if code_challenge:
        fields.extend([("code_challenge", code_challenge), ("code_challenge_method", "S256")])
    reserved = {k for k, _ in fields}
    for key, value in sorted((extra or {}).items()):
        if key not in reserved:
            fields.append((key, value))
    base = str_field(flow, "authorization_url") or ""
    separator = ("" if base.endswith(("?", "&")) else "&") if "?" in base else "?"
    return f"{base}{separator}{form_body(fields)}"


@dataclass(frozen=True, slots=True)
class _CodeClient:
    client_id: str
    client_secret: str | None
    redirect_uri: str | None


def _text(value: object) -> str | None:
    return value if isinstance(value, str) and value != "" else None


class OAuthSession:
    """Token handling of one client (an ``Engine`` owns one): the store, the
    code flow's requests and the single flight per scheme. Its methods that
    do I/O are flows."""

    def __init__(
        self,
        api: object,
        options: object,
        store: object,
        now: Callable[[], float],
        timeout_ms: Callable[[], float],
    ) -> None:
        self._api = api
        self._options = options
        self._now = now
        self._timeout_ms = timeout_ms
        get, put, delete = (getattr(store, name, None) for name in ("get", "set", "delete"))
        self.store: object = (
            store if callable(get) and callable(put) and callable(delete) else MemoryTokenStore()
        )
        self._attempts: dict[str, PendingAuthorization] = {}
        self._generations: dict[str, int] = {}
        self._logging_out: dict[str, int] = {}
        self._attempt_lock = threading.RLock()
        self._monotonic: Callable[[], float] = lambda: time.monotonic() * 1000

    # ----------------------------------------------------------- lookups

    def flow_of(self, scheme: str) -> Mapping[str, object] | str:
        """The authorization-code flow of ``scheme``, or the reason there is none."""
        found = next(
            (s for s in items_of(field_of(self._api, "auth")) if field_of(s, "name") == scheme), None
        )
        if not is_record(found):
            return f"the API descriptor defines no scheme named {scheme}"
        flow = found.get("authorization_code") if found.get("kind") == "oauth2" else None
        if not is_record(flow):
            return f"the scheme {scheme} has no authorizationCode flow"
        return flow

    def _client(self, scheme: str) -> _CodeClient | str:
        try:
            configured = getattr(self._options, "auth", None)
            config = configured.get(scheme) if is_record(configured) else None
            if not is_record(config) or config.get("flow") != "authorizationCode":
                return (
                    f'auth.{scheme} must be {{"flow": "authorizationCode", "client_id", '
                    '"client_secret"?, "redirect_uri"?}'
                )
            client_id = _text(config.get("client_id"))
            if client_id is None:
                return f"auth.{scheme} needs a client_id"
            return _CodeClient(
                client_id, _text(config.get("client_secret")), _text(config.get("redirect_uri"))
            )
        except Exception:
            return f"auth.{scheme} could not be read"

    @staticmethod
    def _failure(operation: str, remediation: str) -> Err:
        return Err(error=diagnostic(operation, "AUTH_FAILED", remediation=remediation))

    # ------------------------------------------------------------- helpers

    def _generation(self, scheme: str) -> int:
        return self._generations.get(scheme, 0)

    def _fenced_store(self) -> bool:
        return all(
            callable(getattr(self.store, name, None)) for name in ("set_if_current", "delete_if_current")
        )

    def _token_response_limit(self) -> int:
        limit = getattr(self._options, "max_oauth_token_response_bytes", TOKEN_RESPONSE_BYTES)
        if isinstance(limit, bool) or not isinstance(limit, int) or not 0 < limit <= 9_007_199_254_740_991:
            raise TokenError("max_oauth_token_response_bytes must be a positive safe integer.")
        return limit

    def _remove(self, handle: str, pending: PendingAuthorization) -> None:
        pending.verifier = None
        pending.dispose()
        self._attempts.pop(handle, None)

    def _cancel(self, handle: str, pending: PendingAuthorization) -> None:
        pending.canceled = True
        pending.verifier = None
        if not pending.claimed:
            self._remove(handle, pending)

    def _sweep(self) -> None:
        for handle, pending in list(self._attempts.items()):
            if self._monotonic() - pending.started >= TTL_MS:
                self._cancel(handle, pending)

    def _current(self, pending: PendingAuthorization) -> bool:
        with self._attempt_lock:
            if (
                pending.canceled
                or self._logging_out.get(pending.scheme, 0)
                or self._generation(pending.scheme) != pending.generation
                or self._monotonic() - pending.started >= TTL_MS
            ):
                return False
            client, flow = self._client(pending.scheme), self.flow_of(pending.scheme)
            if isinstance(client, str) or isinstance(flow, str):
                return False
            scopes = flow.get("scopes")
            return (
                client.client_id == pending.client_id
                and client.client_secret == pending.client_secret
                and client.redirect_uri == pending.configured_redirect
                and flow.get("authorization_url") == pending.authorization_url
                and flow.get("token_url") == pending.token_url
                and str_field(flow, "refresh_url") == pending.refresh_url
                and is_array(scopes)
                and len(scopes) <= MAX_ITEMS
                and tuple(scopes) == pending.configured_scopes
            )

    def authorization_url(
        self,
        scheme: str,
        *,
        session_binding: str,
        issuer_defense: IssuerDefense,
        s256_supported: bool,
        redirect_uri: str | None = None,
        scopes: Sequence[str] | None = None,
        resource: str | None = None,
        audience: str | None = None,
        account_context: str | None = None,
        extra: Mapping[str, str] | None = None,
    ) -> OAuthResult[AuthorizationAttempt]:
        """Begin a managed attempt; secrets remain in this per-client memory session."""
        operation = f"oauth.{scheme}.authorizationUrl"

        def refuse() -> Err:
            return self._failure(
                operation,
                "Cannot begin authorization: qualify S256 and issuer defense, supply a session binding, valid bounded endpoints and a fenced token store; protected query fields are reserved.",
            )

        with self._attempt_lock:
            self._sweep()
            if len(self._attempts) >= MAX_ATTEMPTS:
                return self._failure(
                    operation,
                    "Authorization attempt capacity is exhausted; finish or cancel an existing attempt.",
                )
            handle: str | None = None
            pending: PendingAuthorization | None = None
            try:
                flow, client = self.flow_of(scheme), self._client(scheme)
                if (
                    isinstance(flow, str)
                    or isinstance(client, str)
                    or not self._fenced_store()
                    or self._logging_out.get(scheme, 0)
                ):
                    return refuse()
                self._token_response_limit()
                redirect = redirect_uri or client.redirect_uri
                configured_scopes = flow.get("scopes")
                if not redirect or not is_array(configured_scopes) or len(configured_scopes) > MAX_ITEMS:
                    return refuse()
                configured_snapshot = tuple(cast(Sequence[str], configured_scopes))
                authorization = str_field(flow, "authorization_url") or ""
                token_url = str_field(flow, "token_url") or ""
                refresh_url = str_field(flow, "refresh_url")
                owned_scopes, owned_extra = capture(
                    chain(
                        (
                            scheme,
                            client.client_id,
                            client.client_secret or "",
                            client.redirect_uri or "",
                            redirect,
                            authorization,
                            token_url,
                            refresh_url or "",
                        ),
                        configured_snapshot,
                    ),
                    session_binding,
                    issuer_defense,
                    s256_supported,
                    scopes,
                    extra,
                    resource,
                    audience,
                    account_context,
                )
                qualified_endpoint(authorization)
                qualified_endpoint(token_url)
                qualified_endpoint(redirect, redirect=True)
                if any(
                    insecure_http(url, getattr(self._options, "allow_insecure_http", False))
                    for url in (authorization, token_url)
                ):
                    return refuse()
                pkce = generate_pkce()
                handle, state = b64url(random_bytes(32)), b64url(random_bytes(32))
                selected = owned_scopes if owned_scopes is not None else configured_snapshot
                pending = PendingAuthorization(
                    scheme,
                    self._generation(scheme),
                    self._monotonic(),
                    session_binding,
                    redirect,
                    client.client_id,
                    client.client_secret,
                    client.redirect_uri,
                    authorization,
                    token_url,
                    refresh_url,
                    configured_snapshot,
                    selected,
                    resource,
                    audience,
                    account_context,
                    issuer_defense,
                    state,
                    pkce.verifier,
                )
                if resource is not None:
                    owned_extra["resource"] = resource
                if audience is not None:
                    owned_extra["audience"] = audience
                url = build_authorization_url(
                    {"authorization_url": authorization, "scopes": selected},
                    client.client_id,
                    redirect,
                    scopes=selected,
                    state=state,
                    code_challenge=pkce.challenge,
                    extra=owned_extra,
                )

                record, attempt_handle = pending, handle

                def expire() -> None:
                    with self._attempt_lock:
                        self._cancel(attempt_handle, record)

                timer = threading.Timer(TTL_MS / 1000, expire)
                timer.daemon = True
                pending.dispose = timer.cancel
                self._attempts[handle] = pending
                timer.start()
                return OAuthOk(AuthorizationAttempt(handle, url, TTL_MS))
            except Exception:
                if handle is not None and pending is not None:
                    self._remove(handle, pending)
                return refuse()

    def exchange_code(
        self,
        scheme: str,
        attempt_handle: str,
        callback_url: str,
        session_binding: str,
    ) -> Flow[OAuthResult[TokenInfo]]:
        operation = f"oauth.{scheme}.exchangeCode"

        def refuse() -> Err:
            return self._failure(
                operation,
                "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new authorization attempt if needed.",
            )

        with self._attempt_lock:
            self._sweep()
            pending = self._attempts.get(attempt_handle)
            if (
                pending is None
                or pending.claimed
                or pending.scheme != scheme
                or pending.binding != session_binding
                or not self._current(pending)
            ):
                return refuse()
            try:
                if (
                    len(callback_url) > MAX_CALLBACK_BYTES
                    or len(callback_url.encode("utf-8", "strict")) > MAX_CALLBACK_BYTES
                    or "#" in callback_url
                ):
                    return refuse()
                target, question, query = callback_url.partition("?")
                if not question or target != pending.redirect_uri:
                    return refuse()
                fields = callback_fields(query)
                if fields.get("state") != pending.state or (
                    pending.defense.kind == "issuerResponse" and fields.get("iss") != pending.defense.value
                ):
                    return refuse()
                code, denied = fields.get("code"), fields.get("error")
                if (
                    (code is None) == (denied is None)
                    or code == ""
                    or denied == ""
                    or (code is not None and ("error_description" in fields or "error_uri" in fields))
                ):
                    return refuse()
                pending.claimed = True
                if denied is not None:
                    self._remove(attempt_handle, pending)
                    return self._failure(
                        operation, "Authorization was denied. Begin a new attempt to authorize again."
                    )
                verifier = pending.verifier
                pending.verifier = None
                if code is None or verifier is None:
                    self._remove(attempt_handle, pending)
                    return refuse()
            except Exception:
                return refuse()
        try:
            if not self._current(pending):
                return refuse()
            fields_list = [
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", pending.redirect_uri),
                ("code_verifier", verifier),
            ]
            stored = yield from self._token_request(
                scheme,
                pending.token_url,
                _CodeClient(pending.client_id, pending.client_secret, pending.redirect_uri),
                fields_list,
                None,
            )
            if not self._current(pending):
                return refuse()

            def fence(_current: StoredToken | None) -> bool:
                return self._current(pending)

            accepted = yield from invoke(self._method("set_if_current"), scheme, stored, fence)
            if accepted is not True or not self._current(pending):
                return refuse()
            return OAuthOk(_info(scheme, stored))
        except Exception:
            return self._failure(
                operation,
                "Authorization exchange did not complete. The code may have been consumed; begin a new authorization attempt.",
            )
        finally:
            with self._attempt_lock:
                pending.canceled = True
                self._remove(attempt_handle, pending)

    def cancel_authorization(
        self, scheme: str, attempt_handle: str, session_binding: str
    ) -> OAuthResult[None]:
        with self._attempt_lock:
            pending = self._attempts.get(attempt_handle)
            if pending is None or pending.scheme != scheme or pending.binding != session_binding:
                return self._failure(
                    f"oauth.{scheme}.cancelAuthorization",
                    "Authorization attempt is unavailable or belongs to a different session.",
                )
            self._cancel(attempt_handle, pending)
            return OAuthOk(None)

    def logout(self, scheme: str) -> Flow[OAuthResult[None]]:
        with self._attempt_lock:
            self._generations[scheme] = self._generation(scheme) + 1
            self._logging_out[scheme] = self._logging_out.get(scheme, 0) + 1
            for handle, pending in list(self._attempts.items()):
                if pending.scheme == scheme:
                    self._cancel(handle, pending)
        try:
            yield from invoke(self._method("delete"), scheme)
            return OAuthOk(None)
        except Exception:
            return self._failure(
                f"oauth.{scheme}.logout", "The token store failed while removing local credentials."
            )
        finally:
            with self._attempt_lock:
                self._logging_out[scheme] -= 1

    def low_level_authorization_url(
        self,
        scheme: str,
        redirect_uri: str | None = None,
        scopes: Sequence[str] | None = None,
        state: str | None = None,
        code_challenge: str | None = None,
        extra: Mapping[str, str] | None = None,
    ) -> OAuthResult[str]:
        operation = f"oauth.{scheme}.authorizationUrl"
        flow = self.flow_of(scheme)
        if isinstance(flow, str):
            return self._failure(operation, f"Cannot build an authorization URL: {flow}.")
        client = self._client(scheme)
        if isinstance(client, str):
            return self._failure(operation, f"Cannot build an authorization URL: {client}.")
        target = _text(redirect_uri) or client.redirect_uri
        if target is None:
            return self._failure(
                operation,
                f"Pass redirect_uri, or set redirect_uri in auth.{scheme}; the authorization URL needs one.",
            )
        return OAuthOk(
            build_authorization_url(
                flow,
                client.client_id,
                target,
                scopes=scopes,
                state=state,
                code_challenge=code_challenge,
                extra=extra,
            )
        )

    def low_level_exchange_code(
        self,
        scheme: str,
        code: str | None,
        redirect_uri: str | None = None,
        code_verifier: str | None = None,
    ) -> Flow[OAuthResult[TokenInfo]]:
        """Trade an authorization code for tokens and store them."""
        operation = f"oauth.{scheme}.exchangeCode"
        flow = self.flow_of(scheme)
        if isinstance(flow, str):
            return self._failure(operation, f"Cannot exchange the code: {flow}.")
        client = self._client(scheme)
        if isinstance(client, str):
            return self._failure(operation, f"Cannot exchange the code: {client}.")
        text = _text(code)
        if text is None:
            return self._failure(
                operation, "Pass the authorization code the server redirected back with as code."
            )
        target = _text(redirect_uri) or client.redirect_uri
        if target is None:
            return self._failure(
                operation,
                f"Pass redirect_uri, or set redirect_uri in auth.{scheme}; it must equal the one used for the authorization URL.",
            )
        fields = [("grant_type", "authorization_code"), ("code", text), ("redirect_uri", target)]
        verifier = _text(code_verifier)
        if verifier is not None:
            fields.append(("code_verifier", verifier))
        try:
            stored = yield from self._token_request(
                scheme, str_field(flow, "token_url") or "", client, fields, None
            )
            yield from invoke(self._method("set"), scheme, stored)
        except TokenError as error:
            return self._failure(operation, str(error))
        except Exception:
            return self._failure(operation, f"The token store failed while saving the tokens for {scheme}.")
        return OAuthOk(_info(scheme, stored))

    def refresh(self, scheme: str) -> Flow[OAuthResult[TokenInfo]]:
        """Refresh the stored token of ``scheme`` now (single flight)."""
        operation = f"oauth.{scheme}.refresh"
        generation = self._generation(scheme)
        try:
            yield from self.token(scheme, True)
            stored = yield from self._read(scheme)
            if generation != self._generation(scheme):
                raise TokenError("OAuth session changed; authorize again.")
        except TokenError as error:
            return self._failure(operation, str(error))
        except Exception:
            return self._failure(operation, f"The token store failed while refreshing {scheme}.")
        if not isinstance(stored, StoredToken):
            return self._failure(operation, f"No token is stored for {scheme}.")
        return OAuthOk(_info(scheme, stored))

    # -------------------------------------------------------------- tokens

    def token(self, scheme: str, force: bool = False) -> Flow[str]:
        """The access token to send: the stored one, refreshed first when it
        expires within ``EXPIRY_SKEW_MS`` (or ``force``). Raises ``TokenError``
        whose text is shown in the AUTH_FAILED envelope."""
        stored = yield from self._read(scheme)
        if stored is None:
            raise TokenError(
                f"No OAuth2 token is stored for {scheme}; begin with authorization_url(), then call "
                "exchange_code() with its attempt handle, full callback URL and session binding, or put a token in ClientOptions.token_store."
            )
        fresh = stored.expires_at is None or stored.expires_at > self._now() + EXPIRY_SKEW_MS
        if fresh and not force:
            return stored.access_token
        generation = self._generation(scheme)
        token = yield from shared(
            f"oauth2:refresh:{generation}:{scheme}", lambda: self._refresh_stored(scheme, force, generation)
        )
        return token

    def refresh_after_rejection(self, scheme: str, rejected: str) -> Flow[str | None]:
        """Force a refresh after a 401, when the stored token has a refresh
        token. Returns the new access token, or None when nothing changed."""
        try:
            stored = yield from self._read(scheme)
        except TokenError:
            return None
        if stored is None:
            return None
        # Another call already replaced the rejected token.
        if stored.access_token != rejected:
            return stored.access_token
        if stored.refresh_token is None:
            return None
        try:
            token = yield from self.token(scheme, True)
        except Exception:
            return None
        return token

    def _method(self, name: str) -> Callable[..., object]:
        return cast(Callable[..., object], getattr(self.store, name))

    def _read(self, scheme: str) -> Flow[StoredToken | None]:
        generation = self._generation(scheme)
        try:
            if self._logging_out.get(scheme, 0):
                raise TokenError("OAuth session changed; authorize again.")
            stored = yield from invoke(self._method("get"), scheme)
            if self._logging_out.get(scheme, 0) or generation != self._generation(scheme):
                raise TokenError("OAuth session changed; authorize again.")
        except Exception:
            raise TokenError(f"The token store failed while reading the token for {scheme}.") from None
        return stored if isinstance(stored, StoredToken) and stored.access_token != "" else None

    def _refresh_stored(self, scheme: str, force: bool, generation: int) -> Flow[str]:
        if not self._fenced_store():
            raise TokenError("OAuth refresh requires an atomic fenced token store.")
        stored = yield from self._read(scheme)
        if generation != self._generation(scheme) or self._logging_out.get(scheme, 0):
            raise TokenError("OAuth session changed; authorize again.")
        if stored is None:
            raise TokenError(f"No OAuth2 token is stored for {scheme}.")
        flow = self.flow_of(scheme)
        client = self._client(scheme)
        if isinstance(flow, str) or isinstance(client, str):
            raise TokenError(
                f"Cannot refresh the OAuth2 token for {scheme}: {flow if isinstance(flow, str) else client}."
            )
        if stored.refresh_token is None:
            raise TokenError(
                f"The OAuth2 token for {scheme} "
                + ("has no refresh token" if force else "expired and has no refresh token")
                + "; authorize again with authorization_url() and exchange_code()."
            )
        fields = [("grant_type", "refresh_token"), ("refresh_token", stored.refresh_token)]
        url = str_field(flow, "refresh_url") or str_field(flow, "token_url") or ""
        try:
            renewed = yield from self._token_request(scheme, url, client, fields, stored.refresh_token)
        except TokenError as error:
            if error.rejected:
                # The server no longer accepts the refresh token: forget the tokens.
                with contextlib.suppress(Exception):  # the failure below is what the caller needs
                    yield from invoke(
                        self._method("delete_if_current"),
                        scheme,
                        self._refresh_fence(scheme, generation, stored),
                    )
            raise
        try:
            accepted = yield from invoke(
                self._method("set_if_current"),
                scheme,
                renewed,
                self._refresh_fence(scheme, generation, stored),
            )
            if accepted is not True or generation != self._generation(scheme):
                raise TokenError("OAuth session changed; authorize again.")
        except Exception:
            raise TokenError(
                f"The token store failed while saving the refreshed token for {scheme}."
            ) from None
        return renewed.access_token

    def _refresh_fence(
        self, scheme: str, generation: int, previous: StoredToken
    ) -> Callable[[StoredToken | None], bool]:
        def current(stored: StoredToken | None) -> bool:
            with self._attempt_lock:
                return (
                    self._generation(scheme) == generation
                    and not self._logging_out.get(scheme, 0)
                    and stored is not None
                    and stored.access_token == previous.access_token
                    and stored.refresh_token == previous.refresh_token
                )

        return current

    def _token_request(
        self,
        scheme: str,
        url: str,
        client: _CodeClient,
        fields: list[tuple[str, str]],
        previous: str | None,
    ) -> Flow[StoredToken]:
        """One request to the token endpoint. ``previous`` is the refresh
        token a response without one keeps."""
        limit = self._token_response_limit()
        if insecure_http(url, getattr(self._options, "allow_insecure_http", False)):
            raise TokenError(INSECURE_HTTP_AUTH)
        headers = {"Content-Type": "application/x-www-form-urlencoded", "Accept": "application/json"}
        if client.client_secret is not None:
            pair = f"{percent_encode(client.client_id)}:{percent_encode(client.client_secret)}"
            headers["Authorization"] = f"Basic {b64(utf8(pair))}"
        else:
            fields.append(("client_id", client.client_id))
        outcome = yield from send(
            AttemptRequest(
                url=url,
                method="POST",
                headers=headers,
                body=form_body(fields).encode("ascii"),
                timeout_ms=self._timeout_ms(),
                max_response_bytes=limit,
            )
        )
        if not isinstance(outcome, Answered) or outcome.body is None:
            raise TokenError(
                f"The OAuth2 token endpoint for {scheme} could not be reached; check network access and the token URL."
            )
        decoded = decode_body(outcome.body, outcome.headers, "application/json")
        if not 200 <= outcome.status <= 299:
            code = get_path(decoded.value, "error")
            named = f" ({code})" if isinstance(code, str) and _is_error_code(code) else ""
            error = TokenError(
                f"The OAuth2 token endpoint for {scheme} answered HTTP {outcome.status}{named}; check the code "
                "or refresh token, client credentials and redirect URI."
            )
            error.rejected = 400 <= outcome.status < 500
            raise error
        access = get_path(decoded.value, "access_token")
        if not isinstance(access, str) or access == "":
            raise TokenError(f"The OAuth2 token endpoint for {scheme} returned no access_token.")
        token = StoredToken(access_token=access)
        refresh = get_path(decoded.value, "refresh_token")
        if isinstance(refresh, str) and refresh != "":
            token = replace(token, refresh_token=refresh)
        elif previous is not None:
            token = replace(token, refresh_token=previous)
        lifetime = get_path(decoded.value, "expires_in")
        if is_number(lifetime):
            token = replace(token, expires_at=self._now() + max(0.0, float(lifetime)) * 1000)
        kind = get_path(decoded.value, "token_type")
        if isinstance(kind, str) and kind != "":
            token = replace(token, token_type=kind)
        scope = get_path(decoded.value, "scope")
        if isinstance(scope, str) and scope != "":
            token = replace(token, scope=scope)
        return token


def _is_error_code(code: str) -> bool:
    return 1 <= len(code) <= 40 and all(c == "_" or "a" <= c <= "z" for c in code)


def _info(scheme: str, token: StoredToken) -> TokenInfo:
    return TokenInfo(scheme, token.expires_at, token.scope, token.refresh_token is not None)


class _Helpers:
    """What the sync and async helpers share: the store, PKCE and the URL."""

    def __init__(self, session: OAuthSession, scheme: str) -> None:
        self._session = session
        self.scheme = scheme

    @property
    def token_store(self) -> object:
        """The tokens' store."""
        return self._session.store

    def pkce(self) -> Pkce:
        """Low-level PKCE primitive. Managed authorization generates and owns its own pair."""
        return generate_pkce()

    def authorization_url(
        self,
        *,
        session_binding: str,
        issuer_defense: IssuerDefense,
        s256_supported: bool,
        redirect_uri: str | None = None,
        scopes: Sequence[str] | None = None,
        resource: str | None = None,
        audience: str | None = None,
        account_context: str | None = None,
        extra: Mapping[str, str] | None = None,
    ) -> OAuthResult[AuthorizationAttempt]:
        return self._session.authorization_url(
            self.scheme,
            session_binding=session_binding,
            issuer_defense=issuer_defense,
            s256_supported=s256_supported,
            redirect_uri=redirect_uri,
            scopes=scopes,
            resource=resource,
            audience=audience,
            account_context=account_context,
            extra=extra,
        )

    def cancel_authorization(self, attempt_handle: str, session_binding: str) -> OAuthResult[None]:
        return self._session.cancel_authorization(self.scheme, attempt_handle, session_binding)

    def low_level_authorization_url(
        self,
        *,
        redirect_uri: str | None = None,
        scopes: Sequence[str] | None = None,
        state: str | None = None,
        code_challenge: str | None = None,
        extra: Mapping[str, str] | None = None,
    ) -> OAuthResult[str]:
        """The URL to send the user to."""
        return self._session.low_level_authorization_url(
            self.scheme, redirect_uri, scopes, state, code_challenge, extra
        )


class OAuthFlow(_Helpers):
    """The helpers of one authorization-code scheme for ``ClientCore``, as
    generated clients expose them."""

    def __init__(
        self,
        session: OAuthSession,
        run: Callable[[Flow[OAuthResult[TokenInfo]]], OAuthResult[TokenInfo]],
        scheme: str,
    ) -> None:
        super().__init__(session, scheme)
        self._run = run

    def exchange_code(
        self,
        attempt_handle: str,
        callback_url: str,
        *,
        session_binding: str,
    ) -> OAuthResult[TokenInfo]:
        return self._run(
            self._session.exchange_code(self.scheme, attempt_handle, callback_url, session_binding)
        )

    def low_level_exchange_code(
        self, code: str, *, redirect_uri: str | None = None, code_verifier: str | None = None
    ) -> OAuthResult[TokenInfo]:
        """Trade the code from the redirect for tokens, stored for later calls."""
        return self._run(
            self._session.low_level_exchange_code(self.scheme, code, redirect_uri, code_verifier)
        )

    def logout(self) -> OAuthResult[None]:
        run = cast(Callable[[Flow[OAuthResult[None]]], OAuthResult[None]], self._run)
        return run(self._session.logout(self.scheme))

    def refresh(self) -> OAuthResult[TokenInfo]:
        """Refresh the stored token now. Calls refresh on their own when it expires."""
        return self._run(self._session.refresh(self.scheme))


class AsyncOAuthFlow(_Helpers):
    """The helpers of one authorization-code scheme for ``AsyncClientCore``."""

    def __init__(
        self,
        session: OAuthSession,
        run: Callable[[Flow[OAuthResult[TokenInfo]]], Awaitable[OAuthResult[TokenInfo]]],
        scheme: str,
    ) -> None:
        super().__init__(session, scheme)
        self._run = run

    async def exchange_code(
        self,
        attempt_handle: str,
        callback_url: str,
        *,
        session_binding: str,
    ) -> OAuthResult[TokenInfo]:
        return await self._run(
            self._session.exchange_code(self.scheme, attempt_handle, callback_url, session_binding)
        )

    async def low_level_exchange_code(
        self, code: str, *, redirect_uri: str | None = None, code_verifier: str | None = None
    ) -> OAuthResult[TokenInfo]:
        """See ``OAuthFlow.exchange_code``."""
        return await self._run(
            self._session.low_level_exchange_code(self.scheme, code, redirect_uri, code_verifier)
        )

    async def logout(self) -> OAuthResult[None]:
        run = cast(Callable[[Flow[OAuthResult[None]]], Awaitable[OAuthResult[None]]], self._run)
        return await run(self._session.logout(self.scheme))

    async def refresh(self) -> OAuthResult[TokenInfo]:
        """See ``OAuthFlow.refresh``."""
        return await self._run(self._session.refresh(self.scheme))
