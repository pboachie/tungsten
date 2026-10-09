# SPDX-License-Identifier: Apache-2.0
"""Idempotency stores (planning/06 "Idempotency store").

``MemoryIdempotencyStore`` is the default: keys live as long as the process.
``FileIdempotencyStore`` persists them to one JSON file, so ``auto`` keys
survive a restart. Both are synchronous and work with ``ClientCore`` and
``AsyncClientCore``; any object with ``get(scope, logical_id)`` and
``put(scope, logical_id, key)`` methods (plain or ``async``) is a store.
"""

from __future__ import annotations

import json
import os
import tempfile
import threading
from collections.abc import Mapping
from pathlib import Path

from ._util import is_record


class MemoryIdempotencyStore:
    """The default store: keys live as long as the process. Use a persistent
    store (``FileIdempotencyStore``, or your own) when a key must survive a
    crash."""

    __slots__ = ("_keys", "_lock")

    def __init__(self) -> None:
        self._keys: dict[tuple[str, str], str] = {}
        self._lock = threading.Lock()

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
        self._lock = threading.Lock()

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
                os.chmod(temporary, 0o600)
                os.replace(temporary, self.path)
            except BaseException:
                Path(temporary).unlink(missing_ok=True)
                raise
