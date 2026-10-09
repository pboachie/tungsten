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
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from typing import Literal, Protocol, cast
from urllib.parse import quote

from ._auth import TokenError
from ._classify import decode_body
from ._effects import Answered, AttemptRequest, Flow, invoke, send, shared
from ._envelope import diagnostic
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


class AsyncTokenStore(Protocol):
    """Like ``TokenStore``, for ``AsyncClientCore`` (which accepts either)."""

    async def get(self, scheme: str) -> StoredToken | None: ...
    async def set(self, scheme: str, token: StoredToken) -> None: ...
    async def delete(self, scheme: str) -> None: ...


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
class OAuthOk[T]:
    value: T
    ok: Literal[True] = True


type OAuthResult[T] = OAuthOk[T] | Err


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

    def authorization_url(
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

    def exchange_code(
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
        try:
            yield from self.token(scheme, True)
            stored = yield from invoke(self._method("get"), scheme)
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
                f"No OAuth2 token is stored for {scheme}; send the user to authorization_url(), then call "
                "exchange_code() with the code, or put a token in ClientOptions.token_store."
            )
        fresh = stored.expires_at is None or stored.expires_at > self._now() + EXPIRY_SKEW_MS
        if fresh and not force:
            return stored.access_token
        token = yield from shared(f"oauth2:refresh:{scheme}", lambda: self._refresh_stored(scheme, force))
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
        try:
            stored = yield from invoke(self._method("get"), scheme)
        except Exception:
            raise TokenError(f"The token store failed while reading the token for {scheme}.") from None
        return stored if isinstance(stored, StoredToken) and stored.access_token != "" else None

    def _refresh_stored(self, scheme: str, force: bool) -> Flow[str]:
        stored = yield from self._read(scheme)
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
                    yield from invoke(self._method("delete"), scheme)
            raise
        try:
            yield from invoke(self._method("set"), scheme, renewed)
        except Exception:
            raise TokenError(
                f"The token store failed while saving the refreshed token for {scheme}."
            ) from None
        return renewed.access_token

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
        """A fresh PKCE pair; pass ``challenge`` to ``authorization_url`` and keep ``verifier`` for ``exchange_code``."""
        return generate_pkce()

    def authorization_url(
        self,
        *,
        redirect_uri: str | None = None,
        scopes: Sequence[str] | None = None,
        state: str | None = None,
        code_challenge: str | None = None,
        extra: Mapping[str, str] | None = None,
    ) -> OAuthResult[str]:
        """The URL to send the user to."""
        return self._session.authorization_url(
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
        self, code: str, *, redirect_uri: str | None = None, code_verifier: str | None = None
    ) -> OAuthResult[TokenInfo]:
        """Trade the code from the redirect for tokens, stored for later calls."""
        return self._run(self._session.exchange_code(self.scheme, code, redirect_uri, code_verifier))

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
        self, code: str, *, redirect_uri: str | None = None, code_verifier: str | None = None
    ) -> OAuthResult[TokenInfo]:
        """See ``OAuthFlow.exchange_code``."""
        return await self._run(self._session.exchange_code(self.scheme, code, redirect_uri, code_verifier))

    async def refresh(self) -> OAuthResult[TokenInfo]:
        """See ``OAuthFlow.refresh``."""
        return await self._run(self._session.refresh(self.scheme))
