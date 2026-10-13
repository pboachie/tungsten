# SPDX-License-Identifier: Apache-2.0
"""The cross-process lease of a ``FileIdempotencyStore`` (``<path>.lock``), the
same file in every runtime: created exclusively, it records its owner
``{"version": 1, "host", "pid", "token"}``. A busy lease is waited for up to
``LEASE_WAIT`` seconds. It is reclaimed only from an owner that is provably
dead (same host, another process, and that process no longer runs); any other
owner (alive, on another host, or unknown) makes ``reserve`` refuse after the
wait. The holder re-checks its token just before it commits the store file,
so a writer whose lease was taken over never overwrites the new owner's key."""

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


class _Gone(Exception):
    """The lease does not exist."""


def _read_owner(lease: Path) -> _Owner | None:
    """The owner of ``lease``; None when it cannot be read or parsed (still
    being written, or not a lease). Raises ``_Gone`` when it does not exist."""
    try:
        text = lease.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise _Gone from None
    except OSError:
        return None
    try:
        value: object = json.loads(text)
    except ValueError:
        return None
    if not is_record(value):
        return None
    host, pid, token = value.get("host"), value.get("pid"), value.get("token")
    if (
        value.get("version") != 1
        or not isinstance(host, str)
        or not host
        or not isinstance(pid, int)
        or isinstance(pid, bool)
        or pid <= 0
        or not isinstance(token, str)
        or not token
    ):
        return None
    return _Owner(host, pid, token)


def _process_gone(pid: int) -> bool:
    """Whether no process with ``pid`` runs on this host. Only POSIX can tell
    without side effects (on Windows ``os.kill`` terminates)."""
    if os.name != "posix":
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    return False


def _provably_dead(owner: _Owner) -> bool:
    return owner.host == socket.gethostname() and owner.pid != os.getpid() and _process_gone(owner.pid)


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
        owner: _Owner | None = None
        try:
            owner = _read_owner(lease)
            retry_now = owner is not None and _provably_dead(owner) and _reclaim(lease, owner, token)
        except _Gone:
            retry_now = True
        left = deadline - time.monotonic()
        if left <= 0:
            raise OSError(_refusal(lease, owner))
        if not retry_now:
            time.sleep(min(delay, left))
            delay = min(delay * 2, 0.1)


def _reclaim(lease: Path, dead: _Owner, token: str) -> bool:
    """Remove a dead owner's lease. It moves the lease aside first: if what it
    moved is not the dead owner's (a live writer took the lease in between),
    it puts it back. Returns whether to retry at once."""
    aside = Path(f"{lease}.{token}.stale")
    try:
        os.rename(lease, aside)
    except FileNotFoundError:
        return True
    except OSError:
        return False
    try:
        try:
            moved = _read_owner(aside)
        except _Gone:
            moved = None
        if moved is not None and moved.token == dead.token:
            return True
        with contextlib.suppress(OSError):
            os.link(aside, lease)
        return False
    finally:
        with contextlib.suppress(OSError):
            aside.unlink()


def _refusal(lease: Path, owner: _Owner | None) -> str:
    if owner is None:
        return f"the idempotency store is locked by another writer ({lease}); try again"
    return (
        f"the idempotency store is locked by another writer ({lease}, process {owner.pid} on {owner.host}); "
        "try again, or remove the lease if that process no longer runs"
    )


def _owns(lease: Path, token: str) -> bool:
    try:
        owner = _read_owner(lease)
    except _Gone:
        return False
    return owner is not None and owner.token == token


def fence(lease: Path, token: str) -> None:
    """Raise when the lease was taken over: the key is then not stored."""
    if not _owns(lease, token):
        raise OSError(
            f"the idempotency store lease {lease} was taken over by another writer; the key was not stored"
        )


def release(lease: Path, token: str) -> None:
    """Remove the lease if it is still this writer's; failures are ignored
    (the outcome is decided)."""
    if _owns(lease, token):
        with contextlib.suppress(OSError):
            lease.unlink()
