"""Regression tests for rescue-grid live state reconstruction fixes.

These tests cover the failure modes described in
``docs/plans/rescue_grid_live_stuck_reproduce.md``:

* Flip detection when the reopen fill reports ``psize_after == 0``.
* Flip detection when the reopen fill reports the wrong ``position_side``
  but the signed quantity / order type clearly indicates an opposite-side entry.
* Recovery detection when a close-all is the last event and the position is flat.
* The far-from-market limit-order gate must not drop rescue orders.
"""

from __future__ import annotations

from typing import Any

import pytest

from live.market_data import _filter_limit_order_creations_by_market_distance
from passivbot import RESCUE_INACTIVE_STATE, reconstruct_rescue_states


EPS = 1e-9


def _fill(
    ts: int,
    side: str,
    position_side: str,
    qty: float,
    price: float,
    psize: float,
    pprice: float = 0.0,
    pnl: float = 0.0,
    fee_paid: float = 0.0,
    pb_order_type: str = "",
    order_type: str = "",
) -> dict[str, Any]:
    return {
        "timestamp": ts,
        "side": side,
        "position_side": position_side,
        "qty": qty,
        "price": price,
        "psize": psize,
        "pprice": pprice,
        "pnl": pnl,
        "fee_paid": fee_paid,
        "pb_order_type": pb_order_type,
        "order_type": order_type,
    }


def _params(rescue_max_flips: int = 5) -> dict[str, dict]:
    return {
        "long": {
            "rescue_enabled": True,
            "rescue_trigger_so_index": -1,
            "dca_max_safety_orders": 3,
            "rescue_grid_step_scale": 1.1,
            "rescue_max_flips": rescue_max_flips,
            "rescue_wallet_exposure_limit": 10.0,
            "rescue_on_terminate": "hold",
        },
        "short": {
            "rescue_enabled": True,
            "rescue_trigger_so_index": -1,
            "dca_max_safety_orders": 3,
            "rescue_grid_step_scale": 1.1,
            "rescue_max_flips": rescue_max_flips,
            "rescue_wallet_exposure_limit": 10.0,
            "rescue_on_terminate": "hold",
        },
    }


def _active_side(states: dict[str, dict]) -> str | None:
    if states["long"]["rescue_active"] or states["long"]["rescue_frozen"]:
        return "long"
    if states["short"]["rescue_active"] or states["short"]["rescue_frozen"]:
        return "short"
    return None


def _arm_long_rescue() -> list[dict[str, Any]]:
    """Build the minimal fill sequence that arms rescue on the long side."""
    return [
        _fill(1, "buy", "long", 1.0, 100.0, 1.0, pprice=100.0),
        _fill(2, "buy", "long", 1.0, 99.0, 2.0, pprice=99.5),
        _fill(3, "buy", "long", 1.0, 98.0, 3.0, pprice=99.0),
        _fill(4, "buy", "long", 1.0, 97.0, 4.0, pprice=98.5),
    ]


def test_flip_detected_when_reopen_psize_is_zero():
    """A flip reopen with a malformed psize_after==0 must still be detected."""
    events = _arm_long_rescue()
    events.append(
        _fill(5, "sell", "long", -4.0, 97.0, 0.0, pnl=-10.0, fee_paid=-1.0)
    )
    events.append(_fill(6, "sell", "short", -2.0, 97.0, 0.0))

    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)

    assert _active_side(states) == "short"
    short = states["short"]
    assert short["rescue_flip_count"] == 1
    assert short["rescue_base_qty"] == pytest.approx(2.0, abs=EPS)
    # Close-all loss (10) + fee (1) folded into debt.
    assert short["rescue_debt"] == pytest.approx(11.0, abs=EPS)
    assert short["rescue_anchor_price"] == pytest.approx(97.0, abs=EPS)


def test_flip_detected_when_reopen_has_wrong_position_side():
    """A flip reopen that reports the old position_side must still be detected.

    The only reliable signal here is the signed quantity (a sell with negative
    qty opens a short position).
    """
    events = _arm_long_rescue()
    events.append(
        _fill(5, "sell", "long", -4.0, 97.0, 0.0, pnl=-10.0, fee_paid=-1.0)
    )
    # Reopen reports position_side='long' even though it is a short entry.
    events.append(_fill(6, "sell", "long", -2.0, 97.0, 0.0))

    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)

    assert _active_side(states) == "short"
    short = states["short"]
    assert short["rescue_flip_count"] == 1
    assert short["rescue_base_qty"] == pytest.approx(2.0, abs=EPS)
    assert short["rescue_debt"] == pytest.approx(11.0, abs=EPS)


def test_flip_detected_when_reopen_has_wrong_position_side_but_tagged():
    """A tagged rescue_flip_entry order type overrides a misreported position_side."""
    events = _arm_long_rescue()
    events.append(
        _fill(5, "sell", "long", -4.0, 97.0, 0.0, pnl=-10.0, fee_paid=-1.0)
    )
    events.append(
        {
            **_fill(6, "sell", "long", -2.0, 97.0, 0.0),
            "pb_order_type": "rescue_flip_entry_short",
        }
    )

    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)

    assert _active_side(states) == "short"
    assert states["short"]["rescue_flip_count"] == 1


def test_recovery_when_close_all_is_last_event_and_position_is_flat():
    """If the stream ends after a close-all and there is no reopen, deactivate."""
    events = _arm_long_rescue()
    events.append(
        _fill(5, "sell", "long", -4.0, 97.0, 0.0, pnl=-10.0, fee_paid=-1.0)
    )

    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)

    assert _active_side(states) is None
    assert states["long"] == {**RESCUE_INACTIVE_STATE, "rescue_side": "long"}
    assert states["short"] == {**RESCUE_INACTIVE_STATE, "rescue_side": "short"}


def test_recovery_after_partial_close_sum_flat_with_gap_event():
    """Partial closes that sum to flat, with a funding event between, recover/flip correctly."""
    events = _arm_long_rescue()
    events.extend(
        [
            _fill(5, "sell", "long", -2.0, 97.0, 2.0, pnl=-4.0, fee_paid=-0.5),
            # Funding-like non-position event (qty 0, psize unchanged).
            _fill(6, "buy", "long", 0.0, 97.0, 2.0, pnl=-0.1, fee_paid=0.0),
            _fill(7, "sell", "long", -2.0, 97.0, 0.0, pnl=-4.0, fee_paid=-0.5),
        ]
    )

    # No opposite-side reopen -> recovery.
    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)
    assert _active_side(states) is None

    # Add the reopen; now it should be a flip.
    events.append(_fill(8, "sell", "short", -2.0, 97.0, 2.0))
    states = reconstruct_rescue_states(events, _params(), balance=100000.0, c_mult=1.0)
    assert _active_side(states) == "short"
    assert states["short"]["rescue_flip_count"] == 1


class _MockMarketSnapshot:
    def __init__(self, last: float):
        self.last = last

    def is_valid(self) -> bool:
        return True


class _MockBotForDistanceGate:
    def __init__(self, threshold: float):
        self.config = {"live": {"limit_order_create_max_market_dist_pct": threshold}}

    def _log_symbol(self, symbol: str | None) -> str:
        return symbol or "unknown"

    def _log_symbols(self, symbols, limit=None):
        if limit:
            return ",".join(list(symbols)[:limit])
        return ",".join(symbols)


def test_distance_gate_keeps_rescue_orders_far_from_market():
    """Rescue orders far beyond ``limit_order_create_max_market_dist_pct`` are kept."""
    symbol = "BTCUSDT"
    market_price = 100.0
    orders = [
        {
            "type": "limit",
            "symbol": symbol,
            "side": "buy",
            "position_side": "short",
            "qty": 0.5,
            "price": market_price * 0.5,  # 50% away
            "pb_order_type": "rescue_recovery_close_short",
        },
        {
            "type": "limit",
            "symbol": symbol,
            "side": "sell",
            "position_side": "short",
            "qty": 0.5,
            "price": market_price * 1.5,  # 50% away
            "pb_order_type": "rescue_reverse_entry_short",
        },
        # A normal limit order at the same distance should be dropped.
        {
            "type": "limit",
            "symbol": symbol,
            "side": "buy",
            "position_side": "short",
            "qty": 0.5,
            "price": market_price * 0.5,
            "pb_order_type": "entry_grid_normal_short",
        },
    ]
    snapshots = {symbol: _MockMarketSnapshot(market_price)}
    bot = _MockBotForDistanceGate(threshold=0.1)

    kept = _filter_limit_order_creations_by_market_distance(bot, orders, snapshots)

    kept_types = {o["pb_order_type"] for o in kept}
    assert "rescue_recovery_close_short" in kept_types
    assert "rescue_reverse_entry_short" in kept_types
    assert "entry_grid_normal_short" not in kept_types
    assert len(kept) == 2


def test_distance_gate_keeps_rescue_orders_with_various_prefixes():
    """Any pb_order_type starting with ``rescue_`` bypasses the gate."""
    symbol = "BTCUSDT"
    market_price = 100.0
    rescue_prefixes = [
        "rescue_recovery_close_short",
        "rescue_reverse_entry_short",
        "rescue_flip_close_long",
        "rescue_flip_entry_long",
    ]
    orders = []
    for i, prefix in enumerate(rescue_prefixes):
        orders.append(
            {
                "type": "limit",
                "symbol": symbol,
                "side": "buy" if i % 2 == 0 else "sell",
                "position_side": "short" if "short" in prefix else "long",
                "qty": 0.1,
                "price": market_price * (0.3 if i % 2 == 0 else 1.7),
                "pb_order_type": prefix,
            }
        )

    snapshots = {symbol: _MockMarketSnapshot(market_price)}
    bot = _MockBotForDistanceGate(threshold=0.1)
    kept = _filter_limit_order_creations_by_market_distance(bot, orders, snapshots)

    assert len(kept) == len(orders)
