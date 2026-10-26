# SPDX-License-Identifier: AGPL-3.0-only
"""Receive signed order webhooks with the generated Python SDK."""

from __future__ import annotations

from orders_sdk import webhooks
from tungsten_runtime import sign_webhook

# Example values only: a real endpoint reads its secrets from configuration.
SECRET = "whsec_gallery_current"
PREVIOUS = "whsec_gallery_previous"
NOW = 1_700_000_000

SHIPPED = b'{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_42","carrier":"parcelpost"}}'
TAMPERED = b'{"id":"evt_1001","type":"order.shipped","data":{"order_id":"ord_43","carrier":"parcelpost"}}'
INCOMPLETE = b'{"id":"evt_1002","type":"order.shipped","data":{"order_id":"ord_44"}}'
ROTATED = b'{"id":"evt_1003","type":"order.shipped","data":{"order_id":"ord_45","carrier":"parcelpost"}}'

seen: set[str] = set()


def claim(key: str) -> bool:
    """Application-owned replay tracking: accept each event id once."""
    if key in seen:
        return False
    seen.add(key)
    return True


def receive(body: bytes, header: str) -> None:
    outcome = webhooks.parse_webhook(
        "order.shipped", body, header, [SECRET, PREVIOUS], now=NOW, claim=claim
    )
    if outcome.ok:
        data = outcome.event.data
        print(f"accepted {outcome.type} {outcome.id}: {data.order_id} via {data.carrier}")
    else:
        print(f"rejected {outcome.reason}")


signed = sign_webhook(SHIPPED, SECRET, NOW)
receive(SHIPPED, signed)
receive(SHIPPED, signed)
receive(TAMPERED, signed)
receive(SHIPPED, sign_webhook(SHIPPED, SECRET, NOW - 600))
receive(INCOMPLETE, sign_webhook(INCOMPLETE, SECRET, NOW))
receive(ROTATED, sign_webhook(ROTATED, PREVIOUS, NOW))
