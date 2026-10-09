# SPDX-License-Identifier: Apache-2.0
"""Raising ergonomics over ``Result`` (planning/05: a throwing variant for
human ergonomics, implemented in the runtime). Agents should keep branching
on ``Result.ok``."""

from __future__ import annotations

from typing import Any

from .types import Diagnostic, Err, Ok


class TungstenError(Exception):
    """Carries the diagnostic envelope of a failed call."""

    def __init__(self, envelope: Diagnostic, partial: Any = None) -> None:
        super().__init__(f"{envelope['category']} ({envelope['operation']}): {envelope['remediation']}")
        #: The envelope, exactly as the non-raising API returns it.
        self.envelope = envelope
        #: The failed result's ``partial``: what the call or macro already
        #: produced (it can hold values the API shows only once).
        self.partial = partial


def unwrap[T](result: Ok[T] | Err) -> T:
    """Return the value of a successful result or raise ``TungstenError``."""
    if isinstance(result, Err):
        raise TungstenError(result.error, result.partial)
    return result.value
