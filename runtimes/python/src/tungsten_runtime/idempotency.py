# SPDX-License-Identifier: Apache-2.0
"""Idempotency stores (planning/06 "Idempotency store").

``MemoryIdempotencyStore`` is the default: keys live as long as the process.
``FileIdempotencyStore`` persists them to one JSON file, so ``auto`` keys
survive a restart. Both expose atomic ``reserve(scope, logical_id, candidate)`` and get/put.
Async clients offload synchronous storage using bounded workers. Legacy custom
get/put stores are serialized within this loaded runtime; shared external
storage must implement atomic reserve.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import os
import tempfile
import threading
from collections.abc import Callable, Mapping
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from ._util import is_record


class MemoryIdempotencyStore:
    """The default store: keys live as long as the process. Use a persistent
    store (``FileIdempotencyStore``, or your own) when a key must survive a
    crash."""

    __slots__ = ("_keys", "_lock")

    def __init__(self) -> None:
        self._keys: dict[tuple[str, str], str] = {}
        self._lock = threading.Lock()

    def reserve(self, scope: str, logical_id: str, candidate: str) -> str:
        with self._lock:
            return self._keys.setdefault((scope, logical_id), candidate)

    def get(self, scope: str, logical_id: str) -> str | None:
        with self._lock:
            return self._keys.get((scope, logical_id))

    def put(self, scope: str, logical_id: str, key: str) -> None:
        with self._lock:
            self._keys[(scope, logical_id)] = key


def _valid_store(value: object) -> dict[str, dict[str, str]] | None:
    if not is_record(value) or value.get("version") != 1:
        return None
    keys = value.get("keys")
    if not is_record(keys):
        return None
    out: dict[str, dict[str, str]] = {}
    for scope, entries in keys.items():
        if not is_record(entries):
            return None
        scoped: dict[str, str] = {}
        for logical_id, key in entries.items():
            if not isinstance(key, str):
                return None
            scoped[logical_id] = key
        out[scope] = scoped
    return out


_file_lock = threading.RLock()


class FileIdempotencyStore:
    """A store persisted to one JSON file (``{"version": 1, "keys": {scope:
    {logical_id: key}}}``, the format of the TypeScript runtime's
    ``FileIdempotencyStore``), so a call retried after a crash reuses its key.

    Writes are serialized within the process and atomic (a temporary file in
    the same directory, then a rename); the file is created with mode 0600
    because keys are never meant to be shown. A file that exists but cannot
    be parsed makes ``get`` and ``put`` raise, and the runtime then refuses
    the call rather than issue a second key for the same intent."""

    __slots__ = ("_lock", "path")

    def __init__(self, path: str | os.PathLike[str]) -> None:
        self.path = Path(path)
        self._lock = threading.RLock()

    def _read(self) -> dict[str, dict[str, str]]:
        try:
            text = self.path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return {}
        try:
            parsed: object = json.loads(text)
        except ValueError as error:
            raise ValueError("the idempotency store file is not valid JSON") from error
        keys = _valid_store(parsed)
        if keys is None:
            raise ValueError("the idempotency store file has an unknown format")
        return keys

    def reserve(self, scope: str, logical_id: str, candidate: str) -> str:
        with _file_lock, self._lock:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            lease = Path(str(self.path) + ".lock")
            lease.mkdir()  # Busy/stale leases fail closed; never steal one.
            try:
                existing = self.get(scope, logical_id)
                if existing:
                    return existing
                self.put(scope, logical_id, candidate)
                return candidate
            finally:
                lease.rmdir()

    def get(self, scope: str, logical_id: str) -> str | None:
        with self._lock:
            return self._read().get(scope, {}).get(logical_id)

    def put(self, scope: str, logical_id: str, key: str) -> None:
        with self._lock:
            keys = self._read()
            keys.setdefault(scope, {})[logical_id] = key
            ordered: Mapping[str, Mapping[str, str]] = {
                s: {i: keys[s][i] for i in sorted(keys[s])} for s in sorted(keys)
            }
            text = json.dumps({"version": 1, "keys": ordered}, indent=2, ensure_ascii=False) + "\n"
            self.path.parent.mkdir(parents=True, exist_ok=True)
            fd, temporary = tempfile.mkstemp(
                prefix=f".{self.path.name}.", suffix=".tmp", dir=self.path.parent
            )
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    handle.write(text)
                    handle.flush()
                    os.fsync(handle.fileno())
                os.chmod(temporary, 0o600)
                os.replace(temporary, self.path)
            except BaseException:
                Path(temporary).unlink(missing_ok=True)
                raise


_legacy_lock = threading.Lock()


def reserve_key(store: Any, scope: str, logical_id: str, candidate: str) -> object:
    reserve = getattr(store, "reserve", None)
    if callable(reserve):
        return reserve(scope, logical_id, candidate)
    if inspect.iscoroutinefunction(store.get) or inspect.iscoroutinefunction(store.put):
        return _reserve_legacy_async(store, scope, logical_id, candidate)
    with _legacy_lock:
        existing = store.get(scope, logical_id)
        if existing:
            return existing
        store.put(scope, logical_id, candidate)
        if store.get(scope, logical_id) != candidate:
            raise ValueError("the key could not be stored")
        return candidate


# Completion, rather than cancellation of the awaiter, releases the capacity.
# No unbounded executor queue can accumulate behind a slow disk.
_workers = ThreadPoolExecutor(max_workers=4, thread_name_prefix="tungsten-store")
_capacity = threading.BoundedSemaphore(4)


async def async_store_call(fn: Callable[[], object]) -> object:
    while not _capacity.acquire(blocking=False):
        await asyncio.sleep(0.001)

    def run() -> object:
        try:
            return fn()
        finally:
            _capacity.release()

    try:
        future = _workers.submit(run)
    except BaseException:
        _capacity.release()
        raise
    return await asyncio.shield(asyncio.wrap_future(future))


async def async_store_method(fn: Callable[..., object], *args: str) -> object:
    """Dispatch only the selected method; other async methods are irrelevant."""
    result = fn(*args) if inspect.iscoroutinefunction(fn) else await async_store_call(lambda: fn(*args))
    return await result if inspect.isawaitable(result) else result


async def async_reserve_key(store: object, scope: str, logical_id: str, candidate: str) -> object:
    reserve = getattr(store, "reserve", None)
    if callable(reserve):
        return await async_store_method(reserve, scope, logical_id, candidate)
    return await _reserve_legacy_async(store, scope, logical_id, candidate)


_legacy_tasks: set[asyncio.Task[str]] = set()


async def _reserve_legacy_async(store: Any, scope: str, logical_id: str, candidate: str) -> str:
    # Keep the transaction lock through a late worker completion after the
    # caller cancels. Otherwise a second client could overwrite its candidate.
    # Cancelled waiters never become retained transactions or submit disk work.
    while not _legacy_lock.acquire(blocking=False):
        await asyncio.sleep(0.001)
    try:
        task = asyncio.create_task(_legacy_transaction(store, scope, logical_id, candidate))
    except BaseException:
        _legacy_lock.release()
        raise
    _legacy_tasks.add(task)

    def completed(task: asyncio.Task[str]) -> None:
        _legacy_tasks.discard(task)
        if not task.cancelled():
            task.exception()  # Retrieve late failures if the caller already left.

    task.add_done_callback(completed)
    return await asyncio.shield(task)


async def _legacy_transaction(store: Any, scope: str, logical_id: str, candidate: str) -> str:
    try:
        existing = await async_store_method(store.get, scope, logical_id)
        if isinstance(existing, str) and existing:
            return existing
        await async_store_method(store.put, scope, logical_id, candidate)
        if await async_store_method(store.get, scope, logical_id) != candidate:
            raise ValueError("the key could not be stored")
        return candidate
    finally:
        _legacy_lock.release()
