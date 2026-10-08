# SPDX-License-Identifier: Apache-2.0
"""``ClientCore`` (sync, ``httpx.Client``) and ``AsyncClientCore`` (async,
``httpx.AsyncClient``): the behaviour of planning/06 with the semantics of
the TypeScript runtime's ``ClientCore``.

Both run the same flows (``_core.Engine``); only the I/O differs. No method
raises for API or transport errors: failures are ``Err`` results carrying
the diagnostic envelope. A bug (in the runtime, or a descriptor or validator
that raises) is reported as an ``UNEXPECTED_RESPONSE`` envelope too.
``AsyncClientCore`` runs on asyncio.

PHASE-4 CONTRACT: the signatures are shared with the Python emitter.
"""

from __future__ import annotations

from collections.abc import AsyncIterator, Iterator, Mapping
from types import TracebackType
from typing import Any, Self, cast

from ._core import Engine
from ._drivers import AsyncDriver, SyncDriver, async_http, sync_http
from ._helpers import safe_id, safe_name
from .types import (
    ApiDescriptor,
    CallOptions,
    ClientOptions,
    Err,
    MacroDescriptor,
    Ok,
    OperationDescriptor,
    Page,
    Predicate,
    PreviewResult,
    Result,
)


def _macro_name(macro: object) -> str:
    return safe_name(macro, "name", "<unknown macro>")


class ClientCore:
    """Synchronous runtime core used by generated clients. One instance per
    client: it owns the HTTP connection pool, the idempotency store, the
    confirmation-token key, the OAuth2 token cache and the registry of
    operations resolvable by id. Close it (or use it as a context manager)
    to release the pool."""

    def __init__(self, api: ApiDescriptor, options: ClientOptions | None = None) -> None:
        self.api = api
        self.options = options if options is not None else ClientOptions()
        self._engine = Engine(api, self.options)
        self._driver = SyncDriver(sync_http(getattr(self.options, "transport", None)))

    def register(self, *operations: OperationDescriptor) -> None:
        """Make operations resolvable by id (verification hooks, endpoint
        previews, macro steps). Operations passed to ``call`` are added too."""
        self._engine.register(*operations)

    def operation(self, operation_id: str) -> OperationDescriptor | None:
        """A registered operation by id."""
        return self._engine.operation(operation_id)

    def macro(self, name: str) -> MacroDescriptor | None:
        """A macro of ``ClientOptions.macros`` by name."""
        return self._engine.macro(name)

    def call(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        """Validate, authenticate, apply idempotency and confirmation rules,
        send with retries and classify the response."""
        try:
            return self._driver.run(self._engine.call(op, args, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "building or sending the request")

    def preview(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        """Run the operation's preview mode: local rendering (no network), a
        dry-run header, or a preview endpoint. Mutating operations get a
        confirmation token bound to these exact arguments for five minutes."""
        try:
            return self._driver.run(self._engine.preview(op, args, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "rendering the preview")

    def pages(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Iterator[Result[Page[Any]]]:
        """Iterate pages (cursor, offset, page number or ``Link`` header);
        stops after the last page or after yielding the first error."""
        try:
            for page in self._driver.iterate(self._engine.pages(op, args, opts)):
                yield cast(Ok[Page[Any]] | Err, page)
        except Exception as error:
            yield self._engine.internal(safe_id(op), error, "paginating")

    def poll(
        self,
        op: OperationDescriptor,
        args: Mapping[str, Any],
        until: Predicate,
        interval_ms: int,
        budget_ms: int,
        opts: CallOptions | None = None,
    ) -> Result[Any]:
        """Call a read operation until ``until`` holds on its body or the
        budget runs out (``Ok.timed_out`` is True with the last answer)."""
        try:
            return self._driver.run(self._engine.poll(op, args, until, interval_ms, budget_ms, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "polling")

    def run_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        """Run a compiled macro: steps in order, each step's result bound to
        its ``as`` name; returns the evaluated output, or the first failing
        step's envelope (naming the macro) with the completed steps' results
        as ``partial``. A ``destructive`` or ``irreversible`` macro needs
        ``confirm`` (a token from ``preview_macro``, or True when
        destructive)."""
        try:
            return self._driver.run(self._engine.run_macro(macro, input, opts))
        except Exception as error:
            return self._engine.internal(_macro_name(macro), error, "running the macro")

    def preview_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        """Preview a macro without sending anything: every step's request
        rendered from a dry evaluation, every step's effects and, unless the
        macro is read-only, a confirmation token bound to the macro and this
        exact input."""
        try:
            return self._driver.run(self._engine.preview_macro(macro, input, opts))
        except Exception as error:
            return self._engine.internal(_macro_name(macro), error, "previewing the macro")

    def close(self) -> None:
        """Release the HTTP connection pool."""
        self._driver.http.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, traceback: TracebackType | None
    ) -> None:
        self.close()


class AsyncClientCore:
    """Asynchronous runtime core with the same semantics as ``ClientCore``."""

    def __init__(self, api: ApiDescriptor, options: ClientOptions | None = None) -> None:
        self.api = api
        self.options = options if options is not None else ClientOptions()
        self._engine = Engine(api, self.options)
        self._driver = AsyncDriver(async_http(getattr(self.options, "transport", None)))

    def register(self, *operations: OperationDescriptor) -> None:
        """Make operations resolvable by id."""
        self._engine.register(*operations)

    def operation(self, operation_id: str) -> OperationDescriptor | None:
        """A registered operation by id."""
        return self._engine.operation(operation_id)

    def macro(self, name: str) -> MacroDescriptor | None:
        """A macro of ``ClientOptions.macros`` by name."""
        return self._engine.macro(name)

    async def call(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        """See ``ClientCore.call``."""
        try:
            return await self._driver.run(self._engine.call(op, args, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "building or sending the request")

    async def preview(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        """See ``ClientCore.preview``."""
        try:
            return await self._driver.run(self._engine.preview(op, args, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "rendering the preview")

    async def pages(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> AsyncIterator[Result[Page[Any]]]:
        """See ``ClientCore.pages``."""
        try:
            async for page in self._driver.iterate(self._engine.pages(op, args, opts)):
                yield cast(Ok[Page[Any]] | Err, page)
        except Exception as error:
            yield self._engine.internal(safe_id(op), error, "paginating")

    async def poll(
        self,
        op: OperationDescriptor,
        args: Mapping[str, Any],
        until: Predicate,
        interval_ms: int,
        budget_ms: int,
        opts: CallOptions | None = None,
    ) -> Result[Any]:
        """See ``ClientCore.poll``."""
        try:
            return await self._driver.run(self._engine.poll(op, args, until, interval_ms, budget_ms, opts))
        except Exception as error:
            return self._engine.internal(safe_id(op), error, "polling")

    async def run_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        """See ``ClientCore.run_macro``."""
        try:
            return await self._driver.run(self._engine.run_macro(macro, input, opts))
        except Exception as error:
            return self._engine.internal(_macro_name(macro), error, "running the macro")

    async def preview_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        """See ``ClientCore.preview_macro``."""
        try:
            return await self._driver.run(self._engine.preview_macro(macro, input, opts))
        except Exception as error:
            return self._engine.internal(_macro_name(macro), error, "previewing the macro")

    async def aclose(self) -> None:
        """Release the HTTP connection pool."""
        await self._driver.http.aclose()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self, kind: type[BaseException] | None, error: BaseException | None, traceback: TracebackType | None
    ) -> None:
        await self.aclose()
