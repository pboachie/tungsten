# SPDX-License-Identifier: Apache-2.0
"""``ClientCore`` (sync) and ``AsyncClientCore`` (async).

PHASE-4 STUB: the signatures are the contract with the Python emitter; every
call returns a TRANSPORT_FAILED envelope. The runtime work package implements
the behaviour of planning/06 with the same semantics as the TypeScript
runtime.
"""

from __future__ import annotations

from collections.abc import AsyncIterator, Iterator, Mapping
from typing import Any

from .types import (
    ApiDescriptor,
    CallOptions,
    ClientOptions,
    Diagnostic,
    Err,
    MacroDescriptor,
    OperationDescriptor,
    Page,
    Predicate,
    PreviewResult,
    Result,
)


def _unimplemented(operation: str) -> Err:
    error: Diagnostic = {
        "status": "error",
        "category": "TRANSPORT_FAILED",
        "operation": operation,
        "http_status": None,
        "code": None,
        "failed_parameter": None,
        "received_value": None,
        "expected": None,
        "remediation": "The tungsten Python runtime is not implemented yet.",
        "retryable": "never",
        "retry_after_ms": None,
        "next_action": None,
        "request_id": None,
        "trace": {"attempts": 0},
    }
    return Err(error=error)


class ClientCore:
    """Synchronous runtime core used by generated clients."""

    def __init__(self, api: ApiDescriptor, options: ClientOptions | None = None) -> None:
        self.api = api
        self.options = options or ClientOptions()

    def register(self, *operations: OperationDescriptor) -> None:
        del operations

    def operation(self, operation_id: str) -> OperationDescriptor | None:
        del operation_id
        return None

    def call(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        del args, opts
        return _unimplemented(op["id"])

    def preview(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        del args, opts
        return _unimplemented(op["id"])

    def pages(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Iterator[Result[Page[Any]]]:
        del args, opts
        yield _unimplemented(op["id"])

    def poll(
        self,
        op: OperationDescriptor,
        args: Mapping[str, Any],
        until: Predicate,
        interval_ms: int,
        budget_ms: int,
        opts: CallOptions | None = None,
    ) -> Result[Any]:
        del args, until, interval_ms, budget_ms, opts
        return _unimplemented(op["id"])

    def run_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        del input, opts
        return _unimplemented(macro["name"])

    def preview_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        del input, opts
        return _unimplemented(macro["name"])

    def close(self) -> None:
        """Release the HTTP connection pool."""


class AsyncClientCore:
    """Asynchronous runtime core with the same semantics as ``ClientCore``."""

    def __init__(self, api: ApiDescriptor, options: ClientOptions | None = None) -> None:
        self.api = api
        self.options = options or ClientOptions()

    def register(self, *operations: OperationDescriptor) -> None:
        del operations

    def operation(self, operation_id: str) -> OperationDescriptor | None:
        del operation_id
        return None

    async def call(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        del args, opts
        return _unimplemented(op["id"])

    async def preview(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        del args, opts
        return _unimplemented(op["id"])

    async def pages(
        self, op: OperationDescriptor, args: Mapping[str, Any], opts: CallOptions | None = None
    ) -> AsyncIterator[Result[Page[Any]]]:
        del args, opts
        yield _unimplemented(op["id"])

    async def poll(
        self,
        op: OperationDescriptor,
        args: Mapping[str, Any],
        until: Predicate,
        interval_ms: int,
        budget_ms: int,
        opts: CallOptions | None = None,
    ) -> Result[Any]:
        del args, until, interval_ms, budget_ms, opts
        return _unimplemented(op["id"])

    async def run_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[Any]:
        del input, opts
        return _unimplemented(macro["name"])

    async def preview_macro(
        self, macro: MacroDescriptor, input: Mapping[str, Any], opts: CallOptions | None = None
    ) -> Result[PreviewResult]:
        del input, opts
        return _unimplemented(macro["name"])

    async def aclose(self) -> None:
        """Release the HTTP connection pool."""
