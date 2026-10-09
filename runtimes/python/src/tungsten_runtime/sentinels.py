# SPDX-License-Identifier: Apache-2.0
"""``UNSET``: distinguishes "leave unchanged" (omitted) from ``None``
(explicit JSON null) for optional fields (planning/03 tri-state presence)."""

from __future__ import annotations

from enum import Enum
from typing import Final, Literal


class _UnsetType(Enum):
    UNSET = "UNSET"

    def __repr__(self) -> str:
        return "UNSET"

    def __bool__(self) -> bool:
        return False


UNSET: Final = _UnsetType.UNSET
Unset = Literal[_UnsetType.UNSET]
