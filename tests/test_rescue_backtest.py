"""Integration tests for rescue-grid backtest fill labeling."""

from pathlib import Path

import numpy as np
import pandas as pd
import pytest

try:
    import passivbot_rust as pbr
except Exception:  # pragma: no cover - exercised when the extension is unavailable
    pbr = None

pbr_is_stub = bool(getattr(pbr, "__is_stub__", False)) if pbr is not None else False


@pytest.mark.skipif(pbr is None or pbr_is_stub, reason="passivbot_rust extension not available")
def test_rescue_backtest_emits_rescue_fill_types():
    """A minimal backtest with rescue enabled produces rescue_* entries in fills.csv."""
    from backtest import build_backtest_payload, execute_backtest
    from config_utils import load_config

    root = Path(__file__).resolve().parents[1]
    config = load_config(str(root / "configs" / "rescue_5so_1pct.json"), verbose=False)

    # Run a tiny single-coin backtest on synthetic data so the test is self-contained.
    config["backtest"]["exchanges"] = ["binance"]
    config["backtest"]["coins"] = {"binance": ["BTC"]}
    config["backtest"]["start_date"] = "2021-01-01"
    config["backtest"]["end_date"] = "2021-01-02"
    config["backtest"]["filter_by_min_effective_cost"] = False
    config["live"]["approved_coins"] = {"long": ["BTC"], "short": []}
    config["live"]["warmup_ratio"] = 0.0
    config["live"]["max_warmup_minutes"] = 0
    config["live"]["hedge_mode"] = False
    config["bot"]["short"]["total_wallet_exposure_limit"] = 0.0

    # Shrink spans so the bot can trade immediately on the small synthetic series.
    for side in ("long", "short"):
        side_cfg = config["bot"][side]
        side_cfg["ema_span_0"] = 2
        side_cfg["ema_span_1"] = 5
        side_cfg["forager_volume_ema_span"] = 1
        side_cfg["forager_volatility_ema_span"] = 1
        side_cfg["entry_initial_ema_dist"] = 0.0
        side_cfg["entry_initial_qty_pct"] = 0.1
        side_cfg["dca_price_deviation_pct"] = 0.02
        side_cfg["dca_max_safety_orders"] = 3
        side_cfg["dca_take_profit_pct"] = 0.05
        side_cfg["total_wallet_exposure_limit"] = 1.0
        side_cfg["rescue_enabled"] = True
        side_cfg["rescue_trigger_so_index"] = -1
        side_cfg["n_rescue_fav"] = 3
        side_cfg["n_rescue_rev"] = 2
        side_cfg["rescue_grid_step_scale"] = 1.1
        side_cfg["rescue_recovery_coverage"] = 1.0
        side_cfg["rescue_wallet_exposure_limit"] = 10.0
        side_cfg["rescue_max_flips"] = 1
        side_cfg["rescue_on_terminate"] = "hold"

    n_minutes = 400
    start_ts = 1_609_459_200_000  # 2021-01-01 00:00:00 UTC
    timestamps = np.arange(
        start_ts, start_ts + n_minutes * 60_000, 60_000, dtype=np.int64
    )
    hlcvs = np.zeros((n_minutes, 1, 4), dtype=np.float64)
    for i in range(n_minutes):
        if i < 10:
            price = 100.0 + i * 0.5
        elif i < 30:
            price = 105.0 + (i - 10) * 0.2
        else:
            price = 109.0 - (i - 30) * 0.4
        hlcvs[i, 0, 0] = price + 0.1
        hlcvs[i, 0, 1] = price - 0.1
        hlcvs[i, 0, 2] = price
        hlcvs[i, 0, 3] = 1.0

    btc_usd_prices = np.full(n_minutes, 20_000.0, dtype=np.float64)
    mss = {
        "BTC": {
            "qty_step": 0.001,
            "price_step": 0.1,
            "min_qty": 0.0,
            "min_cost": 0.0,
            "c_mult": 1.0,
            "maker": 0.0002,
            "taker": 0.0005,
            "exchange": "binance",
        },
        "__meta__": {
            "requested_start_ts": int(timestamps[0]),
            "requested_start_date": "2021-01-01",
            "warmup_minutes_requested": 0,
            "warmup_minutes_provided": 0,
        },
    }

    payload = build_backtest_payload(
        hlcvs, mss, config, "binance", btc_usd_prices, timestamps
    )
    fills_arr, equities_array, analysis = execute_backtest(payload, config)

    assert fills_arr is not None and len(fills_arr) > 0
    fdf = pd.DataFrame(
        fills_arr,
        columns=[
            "index",
            "timestamp",
            "coin",
            "pnl",
            "fee_paid",
            "usd_total_balance",
            "btc_cash_wallet",
            "usd_cash_wallet",
            "btc_price",
            "qty",
            "price",
            "psize",
            "pprice",
            "type",
            "liquidity",
            "wallet_exposure",
            "twe_long",
            "twe_short",
            "twe_net",
        ],
    )
    rescue_fills = fdf[fdf["type"].str.contains("rescue", regex=False)]
    assert not rescue_fills.empty, "expected rescue fills to be present"
    for order_type in rescue_fills["type"].unique():
        assert pbr.order_type_snake_to_id(order_type) >= 26
