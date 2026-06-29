# Task 1 — Reproduce the Rescue Grid Live "Stuck Position" Bug

## Your Role

You are a **worker subagent**. Your job is to investigate and reproduce the bug. Do not write final fixes yet. Do not modify production code except to add temporary instrumentation if needed. Return a report with exact findings, code references, and recommended fixes.

## Background

Rescue Grid is a recovery mode for the 3commas-style DCA build. After the last safety order fills underwater, rescue arms and places a two-sided grid. When price reaches the deepest adverse rung, the position flips to the opposite side. After several flips, users report the bot stops gridding and holds a position for days.

The Rust geometry (`passivbot-rust/src/rescue.rs`) and backtest state machine (`passivbot-rust/src/backtest.rs:3106-3600`) are verified. The bug is in the **live path**:

- Live state reconstruction: `src/passivbot.py:590-779` (`reconstruct_rescue_states`).
- Live order creation gating: `src/live/market_data.py:134-179` (`_filter_limit_order_creations_by_market_distance`).
- Orchestrator order emission: `passivbot-rust/src/orchestrator.rs:1519-1782`.

## Goals

1. Build a minimal synthetic fill sequence that demonstrates `reconstruct_rescue_states` getting stuck or deactivating incorrectly.
2. Run a backtest that exercises 3-5 flips and confirm it grids correctly (baseline).
3. Run the live orchestrator snapshot path and show whether grid orders are dropped by the distance gate or other filters.

## Step 1 — Reconstruct state from synthetic fills

Open `src/passivbot.py` and study `reconstruct_rescue_states` (`:590-779`) and `_rescue_state_for_position` (`:10571-10631`).

Write a standalone script (temporary, under `/tmp/opencode/`) that imports `reconstruct_rescue_states` and feeds it synthetic `FillEvent`-like dicts for one symbol.

Suggested scenarios:

### Scenario A: Missing/delayed opposite-side reopen fill

1. Long DCA arms rescue on the last safety order fill (price `100`, avg entry `108`, `b = 0.08`).
2. Price drops; 5 reverse-grid adds fill at `98.40, 96.80, 95.20, 93.60, 92.00`.
3. At `92.00` the whole position closes (close-all fill, `psize_after = 0`).
4. The opposite-side (short) reopen is **not present in the event stream** (simulates exchange delay or order not filled yet).

Expected correct behavior: reconstruction should keep rescue active on the short side once the reopen fill arrives, OR deactivate safely if the position is truly flat.
Actual bug candidate: if the close-all is the last event, the end-of-stream handler (`:764-766`) deactivates rescue. When the reopen eventually arrives, it is treated as a normal DCA initial entry, not a rescue flip.

### Scenario B: Opposite-side reopen with `psize_after == 0`

1. Same as A, but the reopen fill event has `psize_after = 0.0` because the exchange payload is malformed or the position is being reported as flat during the flip.

Check line `:688`: `is_opp_open = is_entry and pside != cur_side and psize_after > eps`. If `psize_after == 0`, the flip is not detected.

### Scenario C: Reopen fill reports the old `position_side`

1. Same as A, but the reopen fill event reports `position_side = "long"` instead of `"short"`.

Check whether reconstruction ever looks at `pb_order_type` / order_type strings. It does not. If the exchange misreports `position_side`, the flip is missed.

### Scenario D: Multiple partial close fills summing to flat

1. Long rescue active.
2. The close-all is executed as two partial closes that together flatten the position.

Check whether both partials set `flat_pending = True` or only the final one. Check whether the reopen detection still works if other events (e.g. funding payments, unrelated fills) appear between the close and the reopen.

Report for each scenario:
- The returned `rescue_active`, `rescue_side`, `rescue_frozen`, `rescue_flip_count`, `rescue_b`, `rescue_base_qty`.
- Whether the function returns the correct active side or deactivates incorrectly.

## Step 2 — Backtest baseline

Use the existing backtest path to run a config that forces several flips.

Suggested approach:
- Use one of the existing rescue configs in `configs/` or create a minimal one under `/tmp/opencode/`.
- Use a synthetic price series or an existing cache if available.
- Set `n_rescue_fav=10`, `n_rescue_rev=5`, `rescue_grid_step_scale=1.1`, `rescue_max_flips=5`, `rescue_wallet_exposure_limit=10.0`.
- Run a backtest and inspect `fills.csv` for `RescueRecoveryClose*`, `RescueReverseEntry*`, `RescueFlipClose*`, `RescueFlipEntry*` order types.
- Verify that grid orders are emitted continuously between flips and that flips occur at the expected trigger prices.

Command hints (from `docs/ai/commands.md`):

```bash
cd /work/passivbot-3commas
source venv/bin/activate
python -m pytest tests/ -k rescue -x -v
```

If no rescue-specific tests exist yet, run the Rust unit tests:

```bash
cd /work/passivbot-3commas/passivbot-rust
cargo test rescue -- --nocapture
```

Report:
- Backtest command used.
- Number of flips observed.
- Whether the backtest grid remained active through all flips.

## Step 3 — Live orchestrator snapshot path

Open `src/passivbot.py:10633-10889` (`calc_ideal_orders_orchestrator_from_snapshot`).

This is the live path: it builds the orchestrator input dict, calls `pbr.compute_ideal_orders_json`, and then converts the returned ideal orders to executable orders via `_to_executable_orders` and `_finalize_reduce_only_orders`.

Build a minimal Python script that:
1. Constructs a `snapshot` dict with one symbol, realistic market price, EMAs, and a position that is in rescue after 2-3 flips.
2. Sets `rescue_active=True`, `rescue_side=...`, `rescue_anchor_price=...`, `rescue_b=...`, `rescue_base_qty=...`, `rescue_debt=...`, `rescue_flip_count=...` directly in the snapshot side inputs (you can monkeypatch `_rescue_state_for_position` to return a fixed state).
3. Calls `calc_ideal_orders_orchestrator_from_snapshot` and inspects the returned `ideal_orders_f`.
4. Applies the distance gate from `src/live/market_data.py:134` (`_filter_limit_order_creations_by_market_distance`) to the output orders and checks how many rescue orders are dropped.

Key variables to vary:
- `live.limit_order_create_max_market_dist_pct` (default `0.8`, try `0.1`, `0.5`, `0.8`, `1.5`).
- `rescue_b` after 2-3 flips (e.g. `0.08 * 1.1^3 ≈ 0.1065`).
- `n_rescue_fav` and `n_rescue_rev`.

Report:
- For each combination, how many rescue recovery closes, rescue reverse entries, and normal orders are emitted before and after the distance gate.
- Whether the distance gate alone can silence the grid.

## Step 4 — Check for other suppressions

While reproducing, also verify these orchestrator-level suppressions:

1. **Side-disabled entry wipe**: `passivbot-rust/src/orchestrator.rs:3152-3208`. If `enabled_long`/`enabled_short` is false, all entries (including rescue reverse-grid adds) are cleared. Does the live path ever pass `enabled_*=False` while a rescue position is open?

2. **Lossy close gate**: `passivbot-rust/src/orchestrator.rs:1114-1192`. Rescue recovery closes are close-order types and can be dropped if they project a loss against peak balance. Does this gate ever drop rescue recovery closes?

3. **Hold-terminate freeze**: `passivbot-rust/src/orchestrator.rs:1734-1758`. When a cap binds with `rescue_on_terminate="hold"`, the orchestrator returns an empty order list until reconstruction sets `rescue_frozen=True`. Does reconstruction's `_apply_caps` (`src/passivbot.py:651-667`) fire reliably?

4. **Reconcile order deduplication**: `src/live/reconciler.py:919-1120`. Does the reconciler ever drop rescue orders because it thinks they are duplicates or because price/qty matching fails?

## Deliverables

Return a report containing:
1. The exact reproduction script(s) you used (paths under `/tmp/opencode/` are fine).
2. For each scenario, the observed vs expected behavior.
3. A ranked list of root causes, from most to least likely.
4. Recommended code locations to fix.
5. Any new test cases that should be added.

Do not modify production files except temporary instrumentation. Do not commit.
