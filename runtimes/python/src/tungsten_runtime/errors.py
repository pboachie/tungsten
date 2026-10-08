# SPDX-License-Identifier: Apache-2.0
"""Raising ergonomics over ``Result``."""

from __future__ import annotations

from .types import Diagnostic, Err, Ok


class TungstenError(Exception):
    """Carries the diagnostic envelope of a failed call."""

    def __init__(self, envelope: Diagnostic) -> None:
        super().__init__(f"{envelope['category']}: {envelope['remediation']}")
        self.envelope = envelope


def unwrap[T](result: Ok[T] | Err) -> T:
    """Return the value of a successful result or raise ``TungstenError``."""
    if isinstance(result, Err):
        raise TungstenError(result.error)
    return result.value
