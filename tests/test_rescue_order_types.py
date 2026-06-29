"""Rescue-grid order type identity tests."""

import pytest

try:
    import passivbot_rust as pbr
except Exception:  # pragma: no cover - exercised when the extension is unavailable
    pbr = None

pbr_is_stub = bool(getattr(pbr, "__is_stub__", False)) if pbr is not None else False


RESCUE_ORDER_TYPES = [
    "rescue_recovery_close_long",
    "rescue_recovery_close_short",
    "rescue_reverse_entry_long",
    "rescue_reverse_entry_short",
    "rescue_flip_close_long",
    "rescue_flip_close_short",
    "rescue_flip_entry_long",
    "rescue_flip_entry_short",
]


@pytest.mark.skipif(pbr is None or pbr_is_stub, reason="passivbot_rust extension not available")
@pytest.mark.parametrize("snake_name", RESCUE_ORDER_TYPES)
def test_rescue_order_type_round_trips(snake_name: str):
    """Rust OrderType enum maps each rescue variant to its snake_case string and back."""
    type_id = pbr.order_type_snake_to_id(snake_name)
    resolved = pbr.order_type_id_to_snake(type_id)
    assert resolved == snake_name
    assert type_id >= 26
