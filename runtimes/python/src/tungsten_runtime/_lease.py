# SPDX-License-Identifier: Apache-2.0
"""The cross-process lease of a ``FileIdempotencyStore`` (``<path>.lock``), the
same file in every runtime: created exclusively, it records its owner
``{"version": 1, "host", "pid", "token"}``. A busy lease is waited for up to
``LEASE_WAIT`` seconds, then ``reserve`` refuses, naming the owner. Only its
owner removes a lease: no writer reclaims another's, because a reclaimer
cannot remove a lease and check whose it was in one step (a delayed reclaimer
could remove a newer live lease). A lease left by a process that died is
removed by hand. The holder re-checks its token just before it commits the
store file, so a writer whose lease was removed never overwrites a key."""

from __future__ import annotations

import contextlib
import json
import os
import secrets
import socket
import time
from dataclasses import dataclass
from pathlib import Path

from ._util import is_record

LEASE_WAIT = 2.0


@dataclass(frozen=True, slots=True)
class _Owner:
    host: str
    pid: int
    token: str


def _read_owner(lease: Path) -> _Owner | None:
    """The owner of ``lease``; None when it cannot be read or parsed (still
    being written, or not a lease). The host may be empty: it is only reported."""
    try:
        value: object = json.loads(lease.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not is_record(value):
        return None
    host, pid, token = value.get("host"), value.get("pid"), value.get("token")
    if (
        value.get("version") != 1
        or not isinstance(host, str)
        or not isinstance(pid, int)
        or isinstance(pid, bool)
        or pid <= 0
        or not isinstance(token, str)
        or not token
    ):
        return None
    return _Owner(host, pid, token)


def acquire(lease: Path) -> str:
    """Take the lease and return its token."""
    token = secrets.token_hex(16)
    mine = json.dumps({"version": 1, "host": socket.gethostname(), "pid": os.getpid(), "token": token}) + "\n"
    deadline = time.monotonic() + LEASE_WAIT
    delay = 0.005
    while True:
        try:
            fd = os.open(lease, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            pass
        else:
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    handle.write(mine)
            except BaseException:
                with contextlib.suppress(OSError):
                    lease.unlink()
                raise
            return token
        left = deadline - time.monotonic()
        if left <= 0:
            raise OSError(_refusal(lease, _read_owner(lease)))
        time.sleep(min(delay, left))
        delay = min(delay * 2, 0.1)


def _refusal(lease: Path, owner: _Owner | None) -> str:
    if owner is None:
        return f"the idempotency store is locked by another writer ({lease}); try again"
    return (
        f"the idempotency store is locked by another writer ({lease}, process {owner.pid} on {json.dumps(owner.host)}); "
        "try again, or remove the lease once that process no longer runs"
    )


def _owns(lease: Path, token: str) -> bool:
    owner = _read_owner(lease)
    return owner is not None and owner.token == token


def fence(lease: Path, token: str) -> None:
    """Raise when the lease is no longer this writer's: the key is then not stored."""
    if not _owns(lease, token):
        raise OSError(
            f"the idempotency store lease {lease} is no longer this writer's; the key was not stored"
        )


def release(lease: Path, token: str) -> None:
    """Remove the lease if it is still this writer's; failures are ignored
    (the outcome is decided)."""
    if _owns(lease, token):
        with contextlib.suppress(OSError):
            lease.unlink()
