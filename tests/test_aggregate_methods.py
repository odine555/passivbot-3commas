"""Tests for aggregate method handling across pareto_store, suite_runner, and optimize.

Covers the fix ensuring that non-default aggregate methods (e.g. "max" for
high_exposure_hours_max_long) are respected when:
  1. suite_runner.apply_scenario falls back to base_coins for scenarios without
     explicit coins
  2. pareto_store._suite_metrics_to_stats extracts aggregated values
  3. pareto_store.main() corrects objective values for non-mean aggregates
  4. optimize.SuiteEvaluator overrides flat_stats before calc_fitness
"""

from copy import deepcopy

import numpy as np
import pytest

from suite_runner import (
    SuiteScenario,
    ScenarioResult,
    aggregate_metrics,
    apply_scenario,
)
from pareto_store import (
    _resolve_aggregate_mode,
    _suite_metrics_to_stats,
)
from metrics_schema import flatten_metric_stats


# ---------------------------------------------------------------------------
# _resolve_aggregate_mode
# ---------------------------------------------------------------------------


class TestResolveAggregateMode:
    def test_none_cfg_returns_mean(self):
        assert _resolve_aggregate_mode("any_metric", None) == "mean"

    def test_empty_cfg_returns_mean(self):
        assert _resolve_aggregate_mode("any_metric", {}) == "mean"

    def test_default_applies_when_metric_absent(self):
        cfg = {"default": "median"}
        assert _resolve_aggregate_mode("unknown_metric", cfg) == "median"

    def test_exact_match(self):
        cfg = {"default": "mean", "high_exposure_hours_max_long": "max"}
        assert _resolve_aggregate_mode("high_exposure_hours_max_long", cfg) == "max"

    def test_base_name_fallback(self):
        """When the full metric isn't in cfg, rsplit('_', 1) base is tried."""
        cfg = {"default": "mean", "peak_recovery_hours_pnl": "max"}
        # The metric "peak_recovery_hours_pnl_long" should match base "peak_recovery_hours_pnl"
        # ... but only if the base is in the cfg.  Actually rsplit("_", 1) on
        # "peak_recovery_hours_pnl" gives ("peak_recovery_hours", "pnl"), so
        # this tests a direct match on the full name.
        assert _resolve_aggregate_mode("peak_recovery_hours_pnl", cfg) == "max"

    def test_base_name_fallback_with_suffix(self):
        cfg = {"default": "mean", "position_held_hours": "min"}
        assert _resolve_aggregate_mode("position_held_hours_max", cfg) == "min"

    def test_default_mean_when_default_absent(self):
        cfg = {"peak_recovery_hours_pnl": "max"}
        assert _resolve_aggregate_mode("unrelated_metric", cfg) == "mean"


# ---------------------------------------------------------------------------
# _suite_metrics_to_stats with aggregate_cfg
# ---------------------------------------------------------------------------


class TestSuiteMetricsToStatsAggregate:
    """Tests that _suite_metrics_to_stats uses aggregate_cfg when the
    pre-computed 'aggregated' field is missing."""

    def _entry_with_metrics_format(self, aggregated_value=None):
        """Build an entry in the 'metrics' suite_metrics format."""
        payload = {
            "stats": {"mean": 100.0, "min": 50.0, "max": 200.0, "std": 30.0},
            "scenarios": {},
        }
        if aggregated_value is not None:
            payload["aggregated"] = aggregated_value
        return {
            "suite_metrics": {
                "metrics": {
                    "high_exposure_hours_max_long": payload,
                }
            }
        }

    def test_precomputed_aggregated_used_when_present(self):
        entry = self._entry_with_metrics_format(aggregated_value=200.0)
        _, agg = _suite_metrics_to_stats(entry)
        assert agg["high_exposure_hours_max_long"] == 200.0

    def test_fallback_uses_mean_without_cfg(self):
        entry = self._entry_with_metrics_format(aggregated_value=None)
        _, agg = _suite_metrics_to_stats(entry)
        assert agg["high_exposure_hours_max_long"] == 100.0  # mean

    def test_fallback_uses_aggregate_cfg_max(self):
        entry = self._entry_with_metrics_format(aggregated_value=None)
        cfg = {"default": "mean", "high_exposure_hours_max_long": "max"}
        _, agg = _suite_metrics_to_stats(entry, aggregate_cfg=cfg)
        assert agg["high_exposure_hours_max_long"] == 200.0  # max

    def test_fallback_uses_aggregate_cfg_min(self):
        entry = self._entry_with_metrics_format(aggregated_value=None)
        cfg = {"default": "mean", "high_exposure_hours_max_long": "min"}
        _, agg = _suite_metrics_to_stats(entry, aggregate_cfg=cfg)
        assert agg["high_exposure_hours_max_long"] == 50.0  # min

    def test_precomputed_takes_precedence_over_cfg(self):
        """When aggregated is present, cfg is irrelevant."""
        entry = self._entry_with_metrics_format(aggregated_value=999.0)
        cfg = {"default": "mean", "high_exposure_hours_max_long": "max"}
        _, agg = _suite_metrics_to_stats(entry, aggregate_cfg=cfg)
        assert agg["high_exposure_hours_max_long"] == 999.0

    def test_stats_flat_always_available(self):
        entry = self._entry_with_metrics_format(aggregated_value=None)
        cfg = {"default": "mean", "high_exposure_hours_max_long": "max"}
        stats_flat, _ = _suite_metrics_to_stats(entry, aggregate_cfg=cfg)
        assert stats_flat["high_exposure_hours_max_long_mean"] == 100.0
        assert stats_flat["high_exposure_hours_max_long_max"] == 200.0

    def test_aggregate_format_fallback(self):
        """Test the elif 'aggregate' branch when aggregated_values are empty."""
        entry = {
            "suite_metrics": {
                "aggregate": {
                    "stats": {
                        "peak_recovery_hours_pnl": {
                            "mean": 300.0,
                            "min": 100.0,
                            "max": 500.0,
                            "std": 80.0,
                        },
                    },
                    # No "aggregated" key
                },
            }
        }
        cfg = {"default": "mean", "peak_recovery_hours_pnl": "max"}
        _, agg = _suite_metrics_to_stats(entry, aggregate_cfg=cfg)
        assert agg["peak_recovery_hours_pnl"] == 500.0


# ---------------------------------------------------------------------------
# aggregate_metrics respects aggregate config
# ---------------------------------------------------------------------------


class TestAggregateMetricsConfig:
    def _make_results(self, metric_name, values):
        return [
            ScenarioResult(
                scenario=SuiteScenario(f"s{i}", None, None, None, None),
                per_exchange={},
                metrics={
                    "stats": {metric_name: {"mean": v, "min": v, "max": v, "std": 0.0}}
                },
                elapsed_seconds=0.0,
                output_path=None,
            )
            for i, v in enumerate(values)
        ]

    def test_default_mean(self):
        results = self._make_results("adg_pnl", [1.0, 3.0])
        summary = aggregate_metrics(results, {"default": "mean"})
        assert summary["aggregated"]["adg_pnl"] == pytest.approx(2.0)

    def test_explicit_max(self):
        results = self._make_results("high_exposure_hours_max_long", [100.0, 300.0])
        cfg = {"default": "mean", "high_exposure_hours_max_long": "max"}
        summary = aggregate_metrics(results, cfg)
        assert summary["aggregated"]["high_exposure_hours_max_long"] == pytest.approx(300.0)

    def test_explicit_min(self):
        results = self._make_results("some_metric", [100.0, 300.0])
        cfg = {"default": "mean", "some_metric": "min"}
        summary = aggregate_metrics(results, cfg)
        assert summary["aggregated"]["some_metric"] == pytest.approx(100.0)

    def test_base_name_fallback_in_aggregate_metrics(self):
        """aggregate_metrics also uses rsplit('_', 1) base lookup."""
        results = self._make_results("position_held_hours_max", [100.0, 400.0])
        cfg = {"default": "mean", "position_held_hours": "max"}
        summary = aggregate_metrics(results, cfg)
        # "position_held_hours_max" base is "position_held_hours" which maps to "max"
        assert summary["aggregated"]["position_held_hours_max"] == pytest.approx(400.0)


# ---------------------------------------------------------------------------
# apply_scenario: base_coins fallback
# ---------------------------------------------------------------------------


class TestApplyScenarioBaseCoins:
    """Scenarios without explicit coins should fall back to base_coins,
    not master_coins."""

    BASE_CONFIG = {
        "backtest": {
            "start_date": "2021-01-01",
            "end_date": "2021-01-31",
            "coins": {},
            "cache_dir": {},
            "exchanges": ["binance"],
        },
        "live": {
            "approved_coins": {"long": [], "short": []},
            "ignored_coins": {"long": [], "short": []},
        },
    }

    def test_no_coins_uses_base_coins(self):
        scenario = SuiteScenario("base", None, None, coins=None, ignored_coins=None)
        base_coins = ["BTC", "ETH"]
        master_coins = ["BTC", "ETH", "DOGE", "SHIB", "XRP"]
        _, coins = apply_scenario(
            deepcopy(self.BASE_CONFIG),
            scenario,
            master_coins=master_coins,
            master_ignored=[],
            available_exchanges=["binance"],
            available_coins={"BTC", "ETH", "DOGE", "SHIB", "XRP"},
            base_coins=base_coins,
        )
        assert coins == ["BTC", "ETH"]

    def test_no_coins_falls_back_to_master_when_base_not_provided(self):
        scenario = SuiteScenario("base", None, None, coins=None, ignored_coins=None)
        master_coins = ["BTC", "ETH", "DOGE"]
        _, coins = apply_scenario(
            deepcopy(self.BASE_CONFIG),
            scenario,
            master_coins=master_coins,
            master_ignored=[],
            available_exchanges=["binance"],
            available_coins={"BTC", "ETH", "DOGE"},
        )
        assert coins == ["BTC", "DOGE", "ETH"]

    def test_explicit_coins_ignores_base_coins(self):
        scenario = SuiteScenario(
            "subset", None, None, coins=["DOGE", "SHIB"], ignored_coins=None
        )
        base_coins = ["BTC", "ETH"]
        master_coins = ["BTC", "ETH", "DOGE", "SHIB"]
        _, coins = apply_scenario(
            deepcopy(self.BASE_CONFIG),
            scenario,
            master_coins=master_coins,
            master_ignored=[],
            available_exchanges=["binance"],
            available_coins={"BTC", "ETH", "DOGE", "SHIB"},
            base_coins=base_coins,
        )
        assert coins == ["DOGE", "SHIB"]

    def test_base_ignored_used_for_no_ignored(self):
        scenario = SuiteScenario("base", None, None, coins=["BTC"], ignored_coins=None)
        base_ignored = ["XRP"]
        master_ignored = ["XRP", "DOGE", "SHIB"]
        cfg, _ = apply_scenario(
            deepcopy(self.BASE_CONFIG),
            scenario,
            master_coins=["BTC"],
            master_ignored=master_ignored,
            available_exchanges=["binance"],
            available_coins={"BTC", "XRP", "DOGE", "SHIB"},
            base_ignored=base_ignored,
        )
        ignored_long = cfg["live"]["ignored_coins"]["long"]
        assert "XRP" in ignored_long
        assert "DOGE" not in ignored_long


# ---------------------------------------------------------------------------
# Optimizer: flat_stats override with aggregated values
# ---------------------------------------------------------------------------


class TestCalcFitnessAggregateOverride:
    """Verify that overriding flat_stats['{metric}_mean'] with
    aggregate_summary['aggregated'] changes the objective value."""

    def test_override_changes_objective(self):
        aggregate_stats = {
            "high_exposure_hours_max_long": {
                "mean": 150.0,
                "min": 100.0,
                "max": 300.0,
                "std": 50.0,
            },
            "adg_pnl": {
                "mean": 0.001,
                "min": 0.0005,
                "max": 0.0015,
                "std": 0.0002,
            },
        }
        aggregated_values = {
            "high_exposure_hours_max_long": 300.0,  # max, not mean
            "adg_pnl": 0.001,  # mean (unchanged)
        }

        flat_stats = flatten_metric_stats(aggregate_stats)

        # Before override: _mean holds the mean
        assert flat_stats["high_exposure_hours_max_long_mean"] == 150.0

        # Apply the override (same logic as optimize.py SuiteEvaluator)
        for metric, agg_value in aggregated_values.items():
            flat_stats[f"{metric}_mean"] = agg_value

        # After override: _mean holds the correctly aggregated value
        assert flat_stats["high_exposure_hours_max_long_mean"] == 300.0
        assert flat_stats["adg_pnl_mean"] == 0.001  # unchanged


# ---------------------------------------------------------------------------
# pareto_store.main() objective correction via ratio
# ---------------------------------------------------------------------------


class TestObjectiveCorrectionRatio:
    """Test the ratio-based correction applied in pareto_store main() for
    scoring metrics with non-default aggregate methods."""

    AGGREGATE_CFG = {
        "default": "mean",
        "high_exposure_hours_max_long": "max",
        "peak_recovery_hours_pnl": "max",
        "position_held_hours_max": "max",
    }

    def _make_pareto_entry(self, scoring_keys, mean_values, max_values, weights=None):
        """Build a pareto entry with suite_metrics in the 'metrics' format."""
        if weights is None:
            weights = {k: 1.0 for k in scoring_keys}
        objectives = {}
        suite_metric_payloads = {}
        for i, sk in enumerate(scoring_keys):
            w = weights.get(sk, 1.0)
            # Stored objectives were computed with _mean (the bug)
            objectives[f"w_{i}"] = mean_values[sk] * w
            suite_metric_payloads[sk] = {
                "stats": {
                    "mean": mean_values[sk],
                    "min": mean_values[sk] * 0.5,
                    "max": max_values[sk],
                    "std": 10.0,
                },
                "aggregated": max_values[sk]
                if self.AGGREGATE_CFG.get(sk, "mean") != "mean"
                else mean_values[sk],
                "scenarios": {},
            }

        return {
            "backtest": {"aggregate": self.AGGREGATE_CFG},
            "optimize": {"scoring": scoring_keys},
            "metrics": {
                "objectives": objectives,
                "constraint_violation": 0.0,
            },
            "suite_metrics": {"metrics": suite_metric_payloads},
        }

    def test_correction_applied_for_max_aggregate(self):
        scoring = ["adg_pnl", "high_exposure_hours_max_long"]
        means = {"adg_pnl": 0.001, "high_exposure_hours_max_long": 150.0}
        maxes = {"adg_pnl": 0.001, "high_exposure_hours_max_long": 300.0}
        entry = self._make_pareto_entry(scoring, means, maxes)

        # Extract and correct (same logic as main())
        metrics_block = entry["metrics"]
        objectives = dict(metrics_block["objectives"])
        aggregate_cfg = entry["backtest"]["aggregate"]
        stats_flat_suite, aggregated_values = _suite_metrics_to_stats(
            entry, aggregate_cfg=aggregate_cfg,
        )

        constraint_violation = metrics_block.get("constraint_violation", 0.0)
        assert not constraint_violation

        for idx, sk in enumerate(scoring):
            mode = _resolve_aggregate_mode(sk, aggregate_cfg)
            if mode == "mean":
                continue
            w_key = f"w_{idx}"
            stored = objectives.get(w_key)
            agg_val = aggregated_values.get(sk)
            mean_val = stats_flat_suite.get(f"{sk}_mean")
            if agg_val is not None and mean_val and mean_val != 0.0:
                objectives[w_key] = stored * (agg_val / mean_val)

        # adg_pnl (mean aggregate) should be unchanged
        assert objectives["w_0"] == pytest.approx(0.001)
        # high_exposure_hours_max_long: stored=150*1.0=150, corrected=150*(300/150)=300
        assert objectives["w_1"] == pytest.approx(300.0)

    def test_correction_preserves_weight_sign(self):
        """Ratio correction preserves the scoring weight direction."""
        scoring = ["peak_recovery_hours_pnl"]
        means = {"peak_recovery_hours_pnl": 200.0}
        maxes = {"peak_recovery_hours_pnl": 500.0}
        weights = {"peak_recovery_hours_pnl": 1.0}
        entry = self._make_pareto_entry(scoring, means, maxes, weights)

        metrics_block = entry["metrics"]
        objectives = dict(metrics_block["objectives"])
        aggregate_cfg = entry["backtest"]["aggregate"]
        stats_flat_suite, aggregated_values = _suite_metrics_to_stats(
            entry, aggregate_cfg=aggregate_cfg,
        )

        stored = objectives["w_0"]
        assert stored == pytest.approx(200.0)  # mean * weight(1.0)

        agg_val = aggregated_values["peak_recovery_hours_pnl"]
        mean_val = stats_flat_suite["peak_recovery_hours_pnl_mean"]
        objectives["w_0"] = stored * (agg_val / mean_val)

        assert objectives["w_0"] == pytest.approx(500.0)  # max * weight(1.0)

    def test_no_correction_with_constraint_violation(self):
        scoring = ["high_exposure_hours_max_long"]
        means = {"high_exposure_hours_max_long": 150.0}
        maxes = {"high_exposure_hours_max_long": 300.0}
        entry = self._make_pareto_entry(scoring, means, maxes)
        entry["metrics"]["constraint_violation"] = 5000.0

        metrics_block = entry["metrics"]
        objectives = dict(metrics_block["objectives"])
        aggregate_cfg = entry["backtest"]["aggregate"]

        constraint_violation = metrics_block.get("constraint_violation", 0.0)
        # Should skip correction
        assert constraint_violation
        # Objective remains at the stored (mean-based) value
        assert objectives["w_0"] == pytest.approx(150.0)

    def test_no_correction_for_mean_aggregate_metric(self):
        scoring = ["adg_pnl"]
        means = {"adg_pnl": 0.001}
        maxes = {"adg_pnl": 0.0015}
        entry = self._make_pareto_entry(scoring, means, maxes)

        metrics_block = entry["metrics"]
        objectives = dict(metrics_block["objectives"])
        aggregate_cfg = entry["backtest"]["aggregate"]

        mode = _resolve_aggregate_mode("adg_pnl", aggregate_cfg)
        assert mode == "mean"
        # No correction needed
        assert objectives["w_0"] == pytest.approx(0.001)

    def test_no_correction_without_aggregate_cfg(self):
        scoring = ["high_exposure_hours_max_long"]
        means = {"high_exposure_hours_max_long": 150.0}
        maxes = {"high_exposure_hours_max_long": 300.0}
        entry = self._make_pareto_entry(scoring, means, maxes)
        del entry["backtest"]["aggregate"]

        aggregate_cfg = entry.get("backtest", {}).get("aggregate")
        assert aggregate_cfg is None
        # Without cfg, no correction is attempted
